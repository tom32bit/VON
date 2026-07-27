//! On-device GGUF inference via `llama.cpp`.
//!
//! Three things make this different from the cloud path:
//!
//! * **Memory.** Weights are `mmap`ed so the OS can page them in and out
//!   instead of the app owning the whole model in its heap — the difference
//!   between running a 4GB model on a phone and being OOM-killed.
//! * **Pacing.** Every decoded token passes through the governor, so a warm
//!   device slows down mid-generation rather than after it.
//! * **Tool calls.** Small local models don't have a reliable native
//!   tool-calling channel, so the agent asks for `<tool_call>` blocks in the
//!   text and we parse them out here. The agent loop can't tell the difference.

use std::path::PathBuf;

use crate::error::{Result, VonError};
use crate::governor;
use crate::llm::{
    Completion,
    Delta,
    Engine,
    GenerateRequest,
    Role,
    StopReason,
    ToolCall,
    ToolSpec,
    Usage,
};

/// Emitted by the model to request a tool. Chosen over a bare JSON object
/// because it survives the model wrapping output in prose or a code fence.
const TOOL_OPEN: &str = "<tool_call>";
const TOOL_CLOSE: &str = "</tool_call>";

pub struct LocalEngine {
    model_path: PathBuf,
    context_len: u32,
}

impl LocalEngine {
    pub fn new() -> Result<Self> {
        let selected = crate::db::get_setting("local_model_id")?
            .ok_or_else(|| VonError::Model("no local model selected in the Model Hub".into()))?;
        let row = crate::db::get_local_model(&selected)?
            .ok_or_else(|| VonError::Model(format!("local model '{selected}' is not registered")))?;
        if !row.downloaded || row.file_path.is_empty() {
            return Err(VonError::Model(format!(
                "'{}' has not finished downloading",
                row.name
            )));
        }
        let path = PathBuf::from(&row.file_path);
        if !path.is_file() {
            return Err(VonError::NotFound(format!(
                "model file for '{}' is missing; re-download it",
                row.name
            )));
        }
        Ok(Self {
            model_path: path,
            context_len: row.context_len.clamp(512, 32_768) as u32,
        })
    }
}

impl Engine for LocalEngine {
    fn generate(
        &self,
        request: &GenerateRequest,
        on_delta: &mut dyn FnMut(Delta),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Completion> {
        // The governor gets the final say on whether local inference may run.
        governor::check_can_run_local()?;
        let _guard = governor::begin_inference();

        // The prompt is rendered inside the backend, because building it needs
        // the loaded model: each GGUF carries its own chat template and using
        // the wrong one is not a cosmetic problem — the model mimics a format
        // it was never trained on and never emits a stop token we recognise.
        let raw = backend::run(
            &self.model_path,
            self.context_len,
            request,
            request.max_tokens,
            on_delta,
            cancelled,
        )?;

        let (text, tool_calls) = parse_tool_calls(&raw.text);
        let stop_reason = if cancelled() {
            StopReason::Cancelled
        } else if !tool_calls.is_empty() {
            StopReason::ToolUse
        } else if raw.hit_token_limit {
            StopReason::MaxTokens
        } else {
            StopReason::EndTurn
        };

        Ok(Completion {
            text,
            tool_calls,
            stop_reason,
            usage: Usage {
                input_tokens: raw.prompt_tokens,
                output_tokens: raw.generated_tokens,
            },
            refusal_reason: None,
        })
    }
}

/// Flattens the transcript into `(role, content)` pairs ready for a chat
/// template.
///
/// The `system` turn is folded into the first user turn rather than emitted
/// separately. Not every template supports a system role — Gemma's has only
/// `user` and `model`, and handing it one is either dropped or an error
/// depending on the build. Merging is slightly lossy on templates that *do*
/// support it, but it behaves identically everywhere, which matters more here.
fn build_turns(request: &GenerateRequest) -> Vec<(String, String)> {
    let mut system = request.system.clone();
    if !request.tools.is_empty() {
        system.push_str(&tool_protocol(&request.tools));
    }

    let mut turns: Vec<(String, String)> = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::System => turns.push(("user".into(), message.text.clone())),
            Role::User => turns.push(("user".into(), message.text.clone())),
            Role::Assistant => {
                let mut body = message.text.clone();
                // Replay prior tool calls in the syntax we ask for, so the
                // model's own history reads back consistently.
                for call in &message.tool_calls {
                    body.push_str(&format!(
                        "\n{TOOL_OPEN}{}{TOOL_CLOSE}",
                        serde_json::json!({ "name": call.name, "arguments": call.arguments })
                    ));
                }
                turns.push(("assistant".into(), body));
            }
            Role::Tool => {
                for result in &message.tool_results {
                    let label = if result.is_error { "ERROR" } else { "RESULT" };
                    turns.push((
                        "user".into(),
                        format!("[{label} from {}]\n{}", result.name, result.content),
                    ));
                }
            }
        }
    }

    if !system.is_empty() {
        match turns.iter_mut().find(|(role, _)| role == "user") {
            Some((_, text)) => *text = format!("{system}\n\n{text}"),
            None => turns.push(("user".into(), system)),
        }
    }

    // Templates alternate strictly; a leading assistant turn breaks some of
    // them outright.
    while turns.first().map(|(r, _)| r != "user").unwrap_or(false) {
        turns.remove(0);
    }
    turns
}

/// ChatML fallback, used only when a GGUF carries no template of its own.
///
/// Hardcoding this for every model was the original bug: Gemma-2 was fed
/// ChatML, mimicked it back, and emitted a literal `<|im_end|>` that is not a
/// token it can stop on — so generation ran to the token budget and the tool
/// call was never in the syntax we asked for.
fn build_prompt_chatml(request: &GenerateRequest) -> String {
    let mut out = String::new();
    for (role, text) in build_turns(request) {
        out.push_str(&format!("<|im_start|>{role}\n{text}<|im_end|>\n"));
    }
    out.push_str("<|im_start|>assistant\n");
    out
}

fn tool_protocol(tools: &[ToolSpec]) -> String {
    let mut out = String::from(
        "\n\n# Tools\n\nYou can call the tools below. To call one, emit exactly:\n\
         <tool_call>{\"name\": \"tool_name\", \"arguments\": {…}}</tool_call>\n\
         Emit nothing else on that line. You may call several in one turn, each \
         in its own block. After calling, stop and wait for the results.\n\n",
    );
    for tool in tools {
        out.push_str(&format!(
            "## {}\n{}\nArguments (JSON Schema): {}\n\n",
            tool.name, tool.description, tool.parameters
        ));
    }
    out
}

/// Turn markers that leak into output when a model imitates a template rather
/// than emitting real control tokens. They are plain text in that case, so
/// nothing stops generation on them and nothing strips them.
const LEAKED_MARKERS: &[&str] = &[
    "<|im_end|>",
    "<|im_start|>",
    "<|eot_id|>",
    "<|end_of_text|>",
    "<|endoftext|>",
    "<end_of_turn>",
    "<start_of_turn>",
    "</s>",
];

fn strip_markers(text: &str) -> String {
    let mut out = text.to_string();
    for marker in LEAKED_MARKERS {
        out = out.replace(marker, "");
    }
    out
}

/// Reads a JSON object as a tool call, accepting the field names models
/// actually use.
fn call_from_json(value: &serde_json::Value) -> Option<ToolCall> {
    let name = value
        .get("name")
        .or_else(|| value.get("tool"))
        .or_else(|| value.get("tool_name"))
        .and_then(|v| v.as_str())?;
    if name.is_empty() {
        return None;
    }
    Some(ToolCall {
        id: format!("local-{}", uuid::Uuid::new_v4()),
        name: name.to_string(),
        arguments: value
            .get("arguments")
            .or_else(|| value.get("parameters"))
            .or_else(|| value.get("args"))
            .or_else(|| value.get("input"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    })
}

/// Finds the end of the JSON object starting at `bytes[start]`, respecting
/// strings and escapes so a brace inside a code string doesn't end it early.
fn json_object_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits generated text into prose and tool calls.
///
/// Deliberately lenient. The documented protocol is `<tool_call>{…}</tool_call>`
/// and strong models follow it, but a 2B model asked to emit an exact syntax
/// will improvise — in testing Gemma-2 produced a markdown fence tagged
/// ```` ```tool_call ```` instead, which the strict parser passed through as
/// prose, so the call silently never ran.
///
/// So: strip leaked turn markers, then scan for a JSON object carrying a
/// `name`, wherever it appears — inside the tags, inside a fence, or bare.
/// Recovering a call the model clearly intended beats failing the whole turn.
fn parse_tool_calls(raw: &str) -> (String, Vec<ToolCall>) {
    let cleaned = strip_markers(raw);
    let bytes = cleaned.as_bytes();

    let mut prose = String::new();
    let mut calls = Vec::new();
    let mut cursor = 0usize;

    while cursor < cleaned.len() {
        let Some(rel) = cleaned[cursor..].find('{') else {
            // Whatever trails the last call is the other half of its wrapper —
            // the closing tag or fence — never prose worth showing.
            prose.push_str(&scrub_wrapper(&cleaned[cursor..]));
            break;
        };
        let open = cursor + rel;

        let Some(close) = json_object_end(bytes, open) else {
            // Unterminated: a truncated final call. Try to salvage it, since
            // the alternative is dumping raw JSON into the chat.
            let tail = &cleaned[open..];
            match serde_json::from_str::<serde_json::Value>(&format!("{tail}}}"))
                .ok()
                .as_ref()
                .and_then(call_from_json)
            {
                Some(call) => calls.push(call),
                None => prose.push_str(tail),
            }
            break;
        };

        let candidate = &cleaned[open..close];
        match serde_json::from_str::<serde_json::Value>(candidate)
            .ok()
            .as_ref()
            .and_then(call_from_json)
        {
            Some(call) => {
                // Drop the scaffolding the model wrapped it in, so the chat
                // shows prose rather than a stray ```tool_call fence.
                prose.push_str(&scrub_wrapper(&cleaned[cursor..open]));
                calls.push(call);
            }
            None => {
                // A JSON object that is not a tool call is just content, and
                // is reproduced byte for byte — only what led up to it gets
                // scrubbed, in case that was a fence the model opened anyway.
                prose.push_str(&scrub_wrapper(&cleaned[cursor..open]));
                prose.push_str(candidate);
            }
        }
        cursor = close;
    }

    (prose.trim().to_string(), calls)
}

/// Fence openers a model reaches for when it decides a tool call is code.
const FENCE_TAGS: &[&str] = &["```tool_call", "```tool-call", "```json", "```tool", "```"];

/// Removes the fence or tag a model wrapped a tool call in, leaving any real
/// prose that preceded it.
///
/// Backticks are treated far more carefully than the `<tool_call>` tags: those
/// are our own protocol and can be deleted wherever they appear, but a fence is
/// ordinary markdown that the agent legitimately emits around code. So a fence
/// is only removed where it is unambiguously scaffolding — a lone one at the
/// very end of the run-up to the call, or a segment that is nothing but
/// wrapper. Stripping every ``` here would silently mangle answers containing
/// code, which is most of them.
fn scrub_wrapper(text: &str) -> String {
    let mut out = text.replace(TOOL_OPEN, "").replace(TOOL_CLOSE, "");

    // Nothing but scaffolding: drop it whole.
    if FENCE_TAGS
        .iter()
        .fold(out.clone(), |acc, tag| acc.replace(tag, ""))
        .trim()
        .is_empty()
    {
        return String::new();
    }

    // Otherwise only the fence immediately adjacent to the call goes; the rest
    // of the prose keeps its markdown.
    let trimmed = out.trim_end();
    for tag in FENCE_TAGS {
        if let Some(stem) = trimmed.strip_suffix(tag) {
            out = stem.to_string();
            break;
        }
    }
    out
}

struct RawOutput {
    text: String,
    prompt_tokens: u32,
    generated_tokens: u32,
    hit_token_limit: bool,
}

// ---- llama.cpp backend --------------------------------------------------

#[cfg(feature = "local-llm")]
mod backend {
    use super::*;
    use std::num::NonZeroU32;
    use std::sync::Arc;

    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
    use llama_cpp_2::sampling::LlamaSampler;
    use once_cell::sync::OnceCell;
    use parking_lot::Mutex;

    /// The backend may only be initialised once per process.
    static BACKEND: OnceCell<LlamaBackend> = OnceCell::new();

    /// Loading a multi-gigabyte model takes seconds even with `mmap`, so the
    /// most recently used one is kept resident. The governor's `shed_memory`
    /// signal drops it when the device gets tight.
    static LOADED: Mutex<Option<(PathBuf, Arc<LlamaModel>)>> = Mutex::new(None);

    fn backend() -> Result<&'static LlamaBackend> {
        BACKEND.get_or_try_init(|| {
            LlamaBackend::init().map_err(|e| VonError::Model(format!("llama backend: {e}")))
        })
    }

    fn load(path: &PathBuf) -> Result<Arc<LlamaModel>> {
        let mut slot = LOADED.lock();
        if let Some((cached_path, model)) = slot.as_ref() {
            if cached_path == path {
                return Ok(model.clone());
            }
        }
        // Drop the previous model before mapping the next one, so peak RSS is
        // one model rather than two.
        *slot = None;

        let params = LlamaModelParams::default().with_n_gpu_layers(0);
        let model = LlamaModel::load_from_file(backend()?, path, &params)
            .map_err(|e| VonError::Model(format!("failed to load model: {e}")))?;
        let model = Arc::new(model);
        *slot = Some((path.clone(), model.clone()));
        Ok(model)
    }

    /// Releases the resident model. Called by the governor under RAM pressure.
    pub fn unload() {
        *LOADED.lock() = None;
    }

    /// Renders the transcript with the template embedded in the GGUF itself.
    ///
    /// Every instruction-tuned model has its own turn markers — Gemma uses
    /// `<start_of_turn>`, Llama-3 `<|start_header_id|>`, Qwen ChatML — and they
    /// are not interchangeable. Reading the template out of the model file is
    /// the only approach that stays correct as the catalog grows.
    fn render_prompt(model: &LlamaModel, request: &GenerateRequest) -> String {
        let turns = build_turns(request);

        let template = match model.chat_template(None) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("model carries no chat template ({e}); falling back to ChatML");
                return build_prompt_chatml(request);
            }
        };

        let chat: std::result::Result<Vec<_>, _> = turns
            .iter()
            .map(|(role, text)| LlamaChatMessage::new(role.clone(), text.clone()))
            .collect();
        let chat = match chat {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("could not build chat messages ({e}); falling back to ChatML");
                return build_prompt_chatml(request);
            }
        };

        match model.apply_chat_template(&template, &chat, true) {
            Ok(prompt) => prompt,
            Err(e) => {
                tracing::warn!("chat template failed ({e}); falling back to ChatML");
                build_prompt_chatml(request)
            }
        }
    }

    pub fn run(
        model_path: &PathBuf,
        context_len: u32,
        request: &GenerateRequest,
        max_new_tokens: u32,
        on_delta: &mut dyn FnMut(Delta),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RawOutput> {
        let model = load(model_path)?;
        let plan = governor::plan();

        let prompt = render_prompt(&model, request);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(context_len))
            .with_n_threads(plan.threads as i32)
            .with_n_threads_batch(plan.threads as i32);

        let mut ctx = model
            .new_context(backend()?, ctx_params)
            .map_err(|e| VonError::Model(format!("failed to create context: {e}")))?;

        // The template already emits the model's BOS marker where it belongs;
        // adding another would shift every position by one.
        let tokens = model
            .str_to_token(&prompt, AddBos::Never)
            .map_err(|e| VonError::Model(format!("tokenisation failed: {e}")))?;
        let prompt_tokens = tokens.len() as u32;

        if prompt_tokens as u32 >= context_len {
            return Err(VonError::Model(format!(
                "prompt is {prompt_tokens} tokens but the context window is {context_len}; \
                 the conversation needs to be shortened"
            )));
        }

        // Prefill.
        let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
        let last = tokens.len() - 1;
        for (i, token) in tokens.iter().enumerate() {
            batch
                .add(*token, i as i32, &[0], i == last)
                .map_err(|e| VonError::Model(format!("batch: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| VonError::Model(format!("prefill failed: {e}")))?;

        let mut text = String::new();
        let mut generated: u32 = 0;
        let mut cursor = batch.n_tokens();
        let budget = max_new_tokens.min(context_len.saturating_sub(prompt_tokens));
        let mut hit_token_limit = false;

        // Greedy: deterministic output, which is what an agent driving tools
        // wants — a sampled token in the middle of a JSON tool call is a
        // wasted turn. Wrapped in a chain because that is the sampler API.
        let mut sampler = LlamaSampler::chain_simple([LlamaSampler::greedy()]);

        // A multi-byte character can straddle two tokens, so pieces are decoded
        // incrementally rather than per token — decoding each token alone would
        // emit replacement characters mid-glyph.
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        while generated < budget {
            if cancelled() {
                break;
            }
            // Re-read the plan every token: a device that heats up mid-answer
            // must slow down now, not on the next prompt.
            let current = governor::plan();
            if current.paused {
                on_delta(Delta::Thinking(
                    "\n[Cooling down — local generation paused]\n".into(),
                ));
                break;
            }

            // Samples from the logits of the batch we last decoded.
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);

            if model.is_eog_token(token) {
                break;
            }

            let piece = model
                .token_to_piece(token, &mut decoder, true, None)
                .unwrap_or_default();
            text.push_str(&piece);
            on_delta(Delta::Text(piece));
            generated += 1;

            // Stop as soon as the model closes a tool call; anything it writes
            // afterwards is hallucinated tool output.
            //
            // Also stop on a leaked turn marker. When a model imitates a
            // template rather than emitting real control tokens, those arrive
            // as ordinary text that `is_eog_token` cannot see — which is how a
            // finished answer previously ran on to the full token budget.
            if text.contains(TOOL_CLOSE)
                || LEAKED_MARKERS.iter().any(|m| text.contains(m))
            {
                break;
            }

            batch.clear();
            batch
                .add(token, cursor, &[0], true)
                .map_err(|e| VonError::Model(format!("batch: {e}")))?;
            cursor += 1;
            ctx.decode(&mut batch)
                .map_err(|e| VonError::Model(format!("decode failed: {e}")))?;

            governor::pace_token();
        }

        if generated >= budget {
            hit_token_limit = true;
        }

        if governor::stats().plan.shed_memory {
            drop(ctx);
            unload();
        }

        Ok(RawOutput {
            text,
            prompt_tokens,
            generated_tokens: generated,
            hit_token_limit,
        })
    }
}

#[cfg(not(feature = "local-llm"))]
mod backend {
    use super::*;

    pub fn unload() {}

    pub fn run(
        _model_path: &PathBuf,
        _context_len: u32,
        _request: &GenerateRequest,
        _max_new_tokens: u32,
        _on_delta: &mut dyn FnMut(Delta),
        _cancelled: &dyn Fn() -> bool,
    ) -> Result<RawOutput> {
        Err(VonError::Model(
            "this build was compiled without the local-llm feature; \
             switch this project to a cloud model"
                .into(),
        ))
    }
}

/// Frees the resident model's memory. Wired to the governor's `shed_memory`
/// signal and to app backgrounding.
pub fn release_model() {
    backend::unload();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, Provider, ToolResult};

    #[test]
    fn parses_a_well_formed_tool_call() {
        let raw = r#"I'll create the file.
<tool_call>{"name": "write_file", "arguments": {"path": "report.py", "content": "print(1)"}}</tool_call>"#;
        let (prose, calls) = parse_tool_calls(raw);
        assert_eq!(prose, "I'll create the file.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write_file");
        assert_eq!(calls[0].arguments["path"], serde_json::json!("report.py"));
    }

    #[test]
    fn parses_several_calls_in_one_turn() {
        let raw = concat!(
            r#"<tool_call>{"name":"write_file","arguments":{"path":"a.py"}}</tool_call>"#,
            "\n",
            r#"<tool_call>{"name":"write_file","arguments":{"path":"b.py"}}</tool_call>"#
        );
        let (_, calls) = parse_tool_calls(raw);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].arguments["path"], serde_json::json!("b.py"));
    }

    #[test]
    fn recovers_a_call_wrapped_in_a_code_fence() {
        let raw = "<tool_call>```json\n{\"name\":\"list_files\",\"arguments\":{}}\n```</tool_call>";
        let (_, calls) = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_files");
    }

    #[test]
    fn recovers_a_call_missing_its_closing_tag() {
        let raw = r#"<tool_call>{"name":"list_files","arguments":{}}"#;
        let (_, calls) = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn accepts_parameters_as_an_alias_for_arguments() {
        let raw = r#"<tool_call>{"name":"read_file","parameters":{"path":"x"}}</tool_call>"#;
        let (_, calls) = parse_tool_calls(raw);
        assert_eq!(calls[0].arguments["path"], serde_json::json!("x"));
    }

    #[test]
    fn unparseable_blocks_are_shown_rather_than_swallowed() {
        let raw = "<tool_call>not json at all</tool_call>";
        let (prose, calls) = parse_tool_calls(raw);
        assert!(calls.is_empty());
        assert!(prose.contains("not json at all"));
    }

    /// Exactly what Gemma-2-2B produced on a real device: a markdown fence
    /// tagged `tool_call` instead of the documented tags, plus a leaked
    /// `<|im_end|>` from the ChatML prompt it was wrongly given.
    #[test]
    fn recovers_a_call_from_a_markdown_fence() {
        let raw = "```tool_call
{\"name\": \"run_python\",                    \"arguments\": {\"code\": \"print(1)\"}}
```
<|im_end|>";
        let (prose, calls) = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1, "prose was: {prose:?}");
        assert_eq!(calls[0].name, "run_python");
        assert_eq!(calls[0].arguments["code"], serde_json::json!("print(1)"));
        assert!(!prose.contains("im_end"), "turn markers must not reach the UI");
        assert!(!prose.contains("```"), "the fence should not survive");
    }

    #[test]
    fn recovers_a_bare_json_call_with_no_wrapper() {
        let raw = r#"I'll run it. {"name":"list_files","arguments":{}}"#;
        let (prose, calls) = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_files");
        assert!(prose.starts_with("I'll run it."));
    }

    #[test]
    fn accepts_the_field_names_models_actually_use() {
        for raw in [
            r#"{"tool":"list_files","args":{}}"#,
            r#"{"tool_name":"list_files","input":{}}"#,
            r#"{"name":"list_files","parameters":{}}"#,
        ] {
            let (_, calls) = parse_tool_calls(raw);
            assert_eq!(calls.len(), 1, "failed on {raw}");
            assert_eq!(calls[0].name, "list_files");
        }
    }

    #[test]
    fn a_brace_inside_a_code_string_does_not_end_the_object() {
        let raw = r#"<tool_call>{"name":"write_file","arguments":{"path":"a.py","content":"d = {}
print(d)"}}</tool_call>"#;
        let (_, calls) = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].arguments["content"].as_str().unwrap().contains("print(d)"));
    }

    #[test]
    fn json_that_is_not_a_tool_call_stays_as_content() {
        let raw = r#"Here is the data: {"sum": 7, "count": 42}"#;
        let (prose, calls) = parse_tool_calls(raw);
        assert!(calls.is_empty());
        assert!(prose.contains("\"sum\": 7"));
    }

    #[test]
    fn leaked_turn_markers_are_stripped_from_plain_prose() {
        let (prose, _) = parse_tool_calls("All done.<end_of_turn>");
        assert_eq!(prose, "All done.");
    }

    #[test]
    fn plain_prose_yields_no_calls() {
        let (prose, calls) = parse_tool_calls("Here is the plan: first, write report.py.");
        assert!(calls.is_empty());
        assert!(prose.starts_with("Here is the plan"));
    }

    #[test]
    fn prompt_renders_chatml_with_the_tool_protocol() {
        let request = GenerateRequest {
            provider: Provider::Local,
            model: "phi-3-mini".into(),
            system: "You are VON.".into(),
            messages: vec![
                ChatMessage::user("write report.py"),
                ChatMessage::tool(vec![ToolResult {
                    call_id: "1".into(),
                    name: "write_file".into(),
                    content: "wrote 42 bytes".into(),
                    is_error: false,
                }]),
            ],
            tools: vec![ToolSpec {
                name: "write_file".into(),
                description: "Write a file".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            max_tokens: 512,
        };
        let prompt = build_prompt_chatml(&request);
        // The system prompt is folded into the first user turn rather than
        // given its own role — see build_turns.
        assert!(!prompt.contains("<|im_start|>system"));
        assert!(prompt.contains("You are VON."));
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("[RESULT from write_file]"));
        assert!(prompt.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn the_system_prompt_leads_the_first_user_turn() {
        let request = GenerateRequest {
            provider: Provider::Local,
            model: "gemma-2-2b-it".into(),
            system: "You are VON.".into(),
            messages: vec![ChatMessage::user("hello")],
            tools: Vec::new(),
            max_tokens: 128,
        };
        let turns = build_turns(&request);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].0, "user");
        assert_eq!(turns[0].1, "You are VON.\n\nhello");
    }

    /// A tool result arriving before any user turn used to leave the transcript
    /// starting on a non-user role, which strict templates reject outright.
    #[test]
    fn a_transcript_never_starts_on_a_non_user_turn() {
        let request = GenerateRequest {
            provider: Provider::Local,
            model: "gemma-2-2b-it".into(),
            system: String::new(),
            messages: vec![
                ChatMessage::assistant("thinking out loud", Vec::new()),
                ChatMessage::user("go"),
            ],
            tools: Vec::new(),
            max_tokens: 128,
        };
        let turns = build_turns(&request);
        assert_eq!(turns.first().map(|(r, _)| r.as_str()), Some("user"));
    }

    /// The parser strips the fence a tool call is wrapped in, but the agent
    /// writes code for a living — a fence around real prose has to survive.
    #[test]
    fn code_fences_in_prose_are_not_eaten() {
        let raw = "Try this:\n```python\nprint(1)\n```\nThen I'll run it.\
                   \n<tool_call>{\"name\":\"run_python\",\"arguments\":{}}</tool_call>";
        let (prose, calls) = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert!(prose.contains("```python"), "prose was: {prose:?}");
        assert!(prose.contains("print(1)"));
    }
}
