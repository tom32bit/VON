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

        let prompt = build_prompt(request);
        let raw = backend::run(
            &self.model_path,
            self.context_len,
            &prompt,
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

/// Renders the transcript in ChatML, which every instruction-tuned GGUF we ship
/// in the catalog understands. Tool definitions and the call protocol go into
/// the system turn, since local models don't have a separate tools channel.
fn build_prompt(request: &GenerateRequest) -> String {
    let mut out = String::new();

    let mut system = request.system.clone();
    if !request.tools.is_empty() {
        system.push_str(&tool_protocol(&request.tools));
    }
    if !system.is_empty() {
        out.push_str(&format!("<|im_start|>system\n{system}<|im_end|>\n"));
    }

    for message in &request.messages {
        match message.role {
            Role::System => {
                out.push_str(&format!("<|im_start|>system\n{}<|im_end|>\n", message.text))
            }
            Role::User => {
                out.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", message.text))
            }
            Role::Assistant => {
                let mut body = message.text.clone();
                // Replay prior tool calls in the same syntax the model emits,
                // so its own history reads back consistently.
                for call in &message.tool_calls {
                    body.push_str(&format!(
                        "\n{TOOL_OPEN}{}{TOOL_CLOSE}",
                        serde_json::json!({ "name": call.name, "arguments": call.arguments })
                    ));
                }
                out.push_str(&format!("<|im_start|>assistant\n{body}<|im_end|>\n"));
            }
            Role::Tool => {
                for result in &message.tool_results {
                    let label = if result.is_error { "ERROR" } else { "RESULT" };
                    out.push_str(&format!(
                        "<|im_start|>user\n[{label} from {}]\n{}<|im_end|>\n",
                        result.name, result.content
                    ));
                }
            }
        }
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

/// Splits generated text into prose and tool calls.
///
/// Deliberately lenient: small models wrap the block in code fences, add stray
/// whitespace, or forget the closing tag on the last call. Recovering a call
/// the model clearly intended beats failing the whole turn.
fn parse_tool_calls(raw: &str) -> (String, Vec<ToolCall>) {
    let mut prose = String::new();
    let mut calls = Vec::new();
    let mut rest = raw;

    while let Some(start) = rest.find(TOOL_OPEN) {
        prose.push_str(&rest[..start]);
        let after = &rest[start + TOOL_OPEN.len()..];

        let (payload, consumed) = match after.find(TOOL_CLOSE) {
            Some(end) => (&after[..end], end + TOOL_CLOSE.len()),
            // Unterminated final block: take the remainder.
            None => (after, after.len()),
        };

        let payload = payload
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
            if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
                calls.push(ToolCall {
                    id: format!("local-{}", uuid::Uuid::new_v4()),
                    name: name.to_string(),
                    arguments: value
                        .get("arguments")
                        .or_else(|| value.get("parameters"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                });
            }
        } else {
            // Not parseable as a call — surface it so the user can see what the
            // model actually produced rather than silently dropping output.
            prose.push_str(payload);
        }

        rest = &after[consumed..];
    }
    prose.push_str(rest);

    (prose.trim().to_string(), calls)
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
    use llama_cpp_2::model::{AddBos, LlamaModel, Special};
    use llama_cpp_2::token::data_array::LlamaTokenDataArray;
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

    pub fn run(
        model_path: &PathBuf,
        context_len: u32,
        prompt: &str,
        max_new_tokens: u32,
        on_delta: &mut dyn FnMut(Delta),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RawOutput> {
        let model = load(model_path)?;
        let plan = governor::plan();

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(context_len))
            .with_n_threads(plan.threads as i32)
            .with_n_threads_batch(plan.threads as i32);

        let mut ctx = model
            .new_context(backend()?, ctx_params)
            .map_err(|e| VonError::Model(format!("failed to create context: {e}")))?;

        let tokens = model
            .str_to_token(prompt, AddBos::Always)
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

            let candidates = LlamaTokenDataArray::from_iter(ctx.candidates(), false);
            let token = ctx.sample_token_greedy(candidates);

            if model.is_eog_token(token) {
                break;
            }

            let piece = model
                .token_to_str(token, Special::Tokenize)
                .unwrap_or_default();
            text.push_str(&piece);
            on_delta(Delta::Text(piece));
            generated += 1;

            // Stop as soon as the model closes a tool call; anything it writes
            // afterwards is hallucinated tool output.
            if text.contains(TOOL_CLOSE) {
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
        _prompt: &str,
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
        let prompt = build_prompt(&request);
        assert!(prompt.contains("<|im_start|>system"));
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("[RESULT from write_file]"));
        assert!(prompt.ends_with("<|im_start|>assistant\n"));
    }
}
