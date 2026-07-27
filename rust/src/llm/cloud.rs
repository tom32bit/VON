//! Cloud providers: Anthropic, OpenAI, Google Gemini and OpenRouter.
//!
//! All three stream Server-Sent Events, but their transcript and tool-calling
//! shapes differ enough that each gets its own request builder and event
//! reducer. The shared parts — the SSE framing, tool-call accumulation, error
//! mapping, cancellation — live here once.
//!
//! Requests carry no sampling parameters. `temperature` / `top_p` / `top_k` are
//! rejected outright by current Anthropic models, and leaving them unset is the
//! right default for an agent everywhere else too.

use std::collections::BTreeMap;

use futures::StreamExt;
use serde_json::{json, Value};

use crate::error::{Result, VonError};
use crate::llm::{
    ChatMessage,
    Completion,
    Delta,
    Engine,
    GenerateRequest,
    Provider,
    Role,
    StopReason,
    ToolSpec,
    Usage,
};
use crate::state;

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// How long the reader waits on one poll before checking for cancellation.
const STREAM_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a stream may send nothing at all before it is treated as hung.
///
/// Generous on purpose: a thinking model legitimately produces no bytes while
/// it reasons, and cutting that short would break the models most worth using.
/// Three minutes is well past any real think and well short of forever.
const STREAM_STALL: std::time::Duration = std::time::Duration::from_secs(180);

pub struct CloudEngine {
    provider: Provider,
    api_key: String,
}

impl CloudEngine {
    pub fn new(provider: Provider, api_key: String) -> Self {
        Self { provider, api_key }
    }

    /// Cheap credential check for the Model Hub's "Test connection" button.
    pub fn test_connection(provider: Provider, api_key: &str, model: &str) -> Result<()> {
        let engine = CloudEngine::new(provider, api_key.to_string());
        let request = GenerateRequest {
            provider,
            model: model.to_string(),
            system: String::new(),
            messages: vec![ChatMessage::user("Reply with the single word: ok")],
            tools: Vec::new(),
            max_tokens: 16,
        };
        engine.generate(&request, &mut |_| {}, &|| false)?;
        Ok(())
    }
}

impl Engine for CloudEngine {
    fn generate(
        &self,
        request: &GenerateRequest,
        on_delta: &mut dyn FnMut(Delta),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Completion> {
        state::block_on(self.stream(request, on_delta, cancelled))?
    }
}

impl CloudEngine {
    /// URL, body and headers for one attempt.
    #[allow(clippy::type_complexity)]
    fn request_shape(
        &self,
        request: &GenerateRequest,
        include_thoughts: bool,
    ) -> Result<(String, Value, Vec<(String, String)>)> {
        Ok(match self.provider {
            Provider::Local => {
                return Err(VonError::Model("local provider is not a cloud engine".into()))
            }
            Provider::Anthropic => (
                ANTHROPIC_URL.to_string(),
                anthropic_body(request),
                vec![
                    ("x-api-key".to_string(), self.api_key.clone()),
                    ("anthropic-version".to_string(), ANTHROPIC_VERSION.to_string()),
                ],
            ),
            Provider::Gemini => (
                format!(
                    "{GEMINI_BASE}/{}:streamGenerateContent?alt=sse",
                    request.model
                ),
                gemini_body(request, include_thoughts),
                // Header rather than a query parameter, so the key never lands
                // in a URL that could be logged.
                vec![("x-goog-api-key".to_string(), self.api_key.clone())],
            ),
            Provider::OpenRouter => (
                OPENROUTER_URL.to_string(),
                openai_body(request, Provider::OpenRouter),
                vec![
                    ("authorization".to_string(), format!("Bearer {}", self.api_key)),
                    // Optional, and good manners: OpenRouter attributes traffic
                    // to the app that sent it rather than to an anonymous key.
                    ("x-title".to_string(), "VON".to_string()),
                    (
                        "http-referer".to_string(),
                        "https://github.com/tom32bit/VON".to_string(),
                    ),
                ],
            ),
            Provider::OpenAi => (
                OPENAI_URL.to_string(),
                openai_body(request, Provider::OpenAi),
                vec![(
                    "authorization".to_string(),
                    format!("Bearer {}", self.api_key),
                )],
            ),
        })
    }

    async fn stream(
        &self,
        request: &GenerateRequest,
        on_delta: &mut dyn FnMut(Delta),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Completion> {
        // Looped because Gemini may need a second attempt: `thinkingConfig` is
        // rejected by models that do not think, and the only way to find out is
        // to be told.
        let mut include_thoughts = self.provider == Provider::Gemini;
        let response = loop {
            let (url, body, headers) = self.request_shape(request, include_thoughts)?;

            let mut builder = state::http()
                .post(&url)
                .header("content-type", "application/json");
            for (key, value) in headers {
                builder = builder.header(key, value);
            }

            let response = builder.json(&body).send().await?;
            let status = response.status();
            if status.is_success() {
                break response;
            }

            let text = response.text().await.unwrap_or_default();

            // The model does not do thinking, so it will not be asked again.
            // Retried rather than reported because the field is ours, not the
            // user's — they did not opt into it and should not have to know.
            if include_thoughts
                && status.as_u16() == 400
                && text.to_lowercase().contains("thinking")
            {
                include_thoughts = false;
                continue;
            }

            // A 404 from a chat endpoint is never a missing URL — the URL is a
            // constant. It means the model is not reachable on this account,
            // which NVIDIA reports as an internal function id the user has no
            // way to interpret. Say what it actually means and what to do.
            if status.as_u16() == 404 {
                return Err(VonError::NotFound(format!(
                    "'{}' is listed by {} but is not available on your account. Pick another                      model — providers list far more than any one key can run.",
                    request.model,
                    self.provider.display_name()
                )));
            }
            return Err(VonError::Provider {
                provider: self.provider.as_str().to_string(),
                status: status.as_u16(),
                body: summarise_error(&text),
            });
        };

        let mut acc = Accumulator::default();
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut idle = std::time::Duration::ZERO;

        'outer: loop {
            // Polled in short slices rather than awaited outright, for two
            // reasons. Stop has to take effect while the model is thinking, not
            // whenever it next decides to speak — a thinking model can send
            // nothing for minutes, and a Stop button that ignores you for that
            // long is not a Stop button. And a connection that has gone quiet
            // without closing is otherwise indistinguishable from one that is
            // working: the loop simply waits forever, which is what "stuck on
            // PLANNING for six minutes" looks like from the outside.
            let chunk = loop {
                if cancelled() {
                    acc.stop_reason = Some(StopReason::Cancelled);
                    break 'outer;
                }
                match tokio::time::timeout(STREAM_POLL, stream.next()).await {
                    Ok(Some(chunk)) => break chunk,
                    Ok(None) => break 'outer,
                    Err(_) => {
                        idle += STREAM_POLL;
                        if idle >= STREAM_STALL {
                            return Err(VonError::Network(format!(
                                "{} accepted the request but sent nothing for {} seconds. The                                  connection is open and idle rather than slow — try again, or                                  switch this VON to another model.",
                                self.provider.display_name(),
                                STREAM_STALL.as_secs()
                            )));
                        }
                    }
                }
            };
            idle = std::time::Duration::ZERO;
            buffer.extend_from_slice(&chunk?);

            // SSE frames are separated by a blank line. Process every complete
            // frame currently in the buffer and keep the remainder.
            while let Some(split) = find_frame_end(&buffer) {
                let frame = buffer.drain(..split.end).collect::<Vec<u8>>();
                let frame = &frame[..split.body_len];
                let text = String::from_utf8_lossy(frame);

                for line in text.lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        // `event:` / `id:` / comment lines carry no payload we
                        // need — the JSON is self-describing.
                        continue;
                    };
                    let data = data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if data == "[DONE]" {
                        break 'outer;
                    }
                    let value: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!("skipping malformed SSE payload: {e}");
                            continue;
                        }
                    };
                    match self.provider {
                        Provider::Anthropic => acc.push_anthropic(&value, on_delta)?,
                        Provider::Gemini => acc.push_gemini(&value, on_delta),
                        Provider::Local => unreachable!(),
                        // OpenAI's frame shape, which is precisely what
                        // OpenRouter exists to normalise everything else into.
                        _ => acc.push_openai(&value, on_delta),
                    }
                }
            }
        }

        Ok(acc.finish())
    }
}

/// Truncates a provider error body before it reaches the UI or a log. Bodies
/// can be large HTML error pages from an intermediary proxy.
fn summarise_error(body: &str) -> String {
    let trimmed = body.trim();
    // Prefer the structured message when the provider sent one.
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(message) = value
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .or_else(|| value.pointer("/error/0/message").and_then(|v| v.as_str()))
            .or_else(|| value.get("message").and_then(|v| v.as_str()))
        {
            return message.chars().take(400).collect();
        }
    }
    trimmed.chars().take(400).collect()
}

struct FrameSplit {
    /// Bytes to drain, including the separator.
    end: usize,
    /// Bytes of actual frame body, excluding the separator.
    body_len: usize,
}

/// Finds the end of the first complete SSE frame. Handles both `\n\n` and
/// `\r\n\r\n`, since intermediaries normalise line endings inconsistently.
fn find_frame_end(buffer: &[u8]) -> Option<FrameSplit> {
    let lf = buffer.windows(2).position(|w| w == b"\n\n");
    let crlf = buffer.windows(4).position(|w| w == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(l), Some(c)) if c < l => Some(FrameSplit {
            end: c + 4,
            body_len: c,
        }),
        (Some(l), _) => Some(FrameSplit {
            end: l + 2,
            body_len: l,
        }),
        (None, Some(c)) => Some(FrameSplit {
            end: c + 4,
            body_len: c,
        }),
        (None, None) => None,
    }
}

/// A tool call being assembled from streamed argument fragments.
#[derive(Default, Clone)]
struct PartialCall {
    id: String,
    name: String,
    args_json: String,
    thought_signature: String,
}

#[derive(Default)]
struct Accumulator {
    text: String,
    /// Keyed by the provider's block/tool index so out-of-order deltas still
    /// land on the right call.
    calls: BTreeMap<usize, PartialCall>,
    stop_reason: Option<StopReason>,
    refusal_reason: Option<String>,
    usage: Usage,
}

impl Accumulator {
    fn finish(self) -> Completion {
        let tool_calls = self
            .calls
            .into_values()
            .filter(|c| !c.name.is_empty())
            .map(|c| crate::llm::ToolCall {
                id: if c.id.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    c.id
                },
                name: c.name,
                thought_signature: c.thought_signature,
                // Always an object, never merely "valid JSON".
                //
                // An empty or truncated fragment means the stream was cut, and
                // an empty object lets the tool report a clear validation error
                // rather than the loop dying on a parse failure. But a model
                // that streams the literal `null` parses *successfully* into
                // Value::Null, which is not an object — and that gets replayed
                // on the next turn as `"arguments": "null"`, which NVIDIA
                // rejects with a 400 that names a message seven turns back.
                arguments: object_or_empty(
                    &serde_json::from_str(c.args_json.trim()).unwrap_or(Value::Null),
                ),
            })
            .collect::<Vec<_>>();

        let stop_reason = self.stop_reason.unwrap_or(if tool_calls.is_empty() {
            StopReason::EndTurn
        } else {
            StopReason::ToolUse
        });

        Completion {
            text: self.text,
            tool_calls,
            stop_reason,
            usage: self.usage,
            refusal_reason: self.refusal_reason,
        }
    }

    // ---- Anthropic ------------------------------------------------------

    fn push_anthropic(&mut self, value: &Value, on_delta: &mut dyn FnMut(Delta)) -> Result<()> {
        match value.get("type").and_then(|v| v.as_str()) {
            Some("message_start") => {
                if let Some(u) = value.pointer("/message/usage") {
                    self.usage.input_tokens = u
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                }
            }
            Some("content_block_start") => {
                let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let block = value.get("content_block");
                if block.and_then(|b| b.get("type")).and_then(|v| v.as_str()) == Some("tool_use") {
                    let id = block
                        .and_then(|b| b.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .and_then(|b| b.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    on_delta(Delta::ToolCallStarted {
                        id: id.clone(),
                        name: name.clone(),
                    });
                    self.calls.insert(
                        index,
                        PartialCall {
                            id,
                            name,
                            ..Default::default()
                        },
                    );
                }
            }
            Some("content_block_delta") => {
                let index = value.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let delta = value.get("delta");
                match delta.and_then(|d| d.get("type")).and_then(|v| v.as_str()) {
                    Some("text_delta") => {
                        if let Some(t) = delta.and_then(|d| d.get("text")).and_then(|v| v.as_str())
                        {
                            self.text.push_str(t);
                            on_delta(Delta::Text(t.to_string()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) =
                            delta.and_then(|d| d.get("thinking")).and_then(|v| v.as_str())
                        {
                            on_delta(Delta::Thinking(t.to_string()));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(fragment) = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(|v| v.as_str())
                        {
                            self.calls.entry(index).or_default().args_json.push_str(fragment);
                        }
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(reason) = value
                    .pointer("/delta/stop_reason")
                    .and_then(|v| v.as_str())
                {
                    self.stop_reason = Some(match reason {
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        "refusal" => {
                            self.refusal_reason = value
                                .pointer("/delta/stop_details/explanation")
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                                .or_else(|| {
                                    value
                                        .pointer("/delta/stop_details/category")
                                        .and_then(|v| v.as_str())
                                        .map(|c| format!("declined ({c})"))
                                });
                            StopReason::Refusal
                        }
                        _ => StopReason::EndTurn,
                    });
                }
                if let Some(n) = value.pointer("/usage/output_tokens").and_then(|v| v.as_u64()) {
                    self.usage.output_tokens = n as u32;
                }
            }
            Some("error") => {
                return Err(VonError::Model(
                    value
                        .pointer("/error/message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("provider streamed an error")
                        .to_string(),
                ))
            }
            _ => {}
        }
        Ok(())
    }

    // ---- OpenAI ---------------------------------------------------------

    fn push_openai(&mut self, value: &Value, on_delta: &mut dyn FnMut(Delta)) {
        if let Some(u) = value.get("usage").filter(|u| !u.is_null()) {
            self.usage.input_tokens =
                u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            self.usage.output_tokens = u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
        }

        let Some(choice) = value.pointer("/choices/0") else {
            return;
        };

        if let Some(content) = choice
            .pointer("/delta/content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            self.text.push_str(content);
            on_delta(Delta::Text(content.to_string()));
        }

        // Some reasoning models stream a separate summary channel.
        if let Some(reasoning) = choice
            .pointer("/delta/reasoning_content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            on_delta(Delta::Thinking(reasoning.to_string()));
        }

        if let Some(calls) = choice
            .pointer("/delta/tool_calls")
            .and_then(|v| v.as_array())
        {
            for call in calls {
                let index = call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let entry = self.calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                    entry.id = id.to_string();
                }
                if let Some(name) = call.pointer("/function/name").and_then(|v| v.as_str()) {
                    if entry.name.is_empty() && !name.is_empty() {
                        entry.name = name.to_string();
                        on_delta(Delta::ToolCallStarted {
                            id: entry.id.clone(),
                            name: name.to_string(),
                        });
                    }
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(|v| v.as_str()) {
                    entry.args_json.push_str(args);
                }
            }
        }

        if let Some(finish) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            self.stop_reason = Some(match finish {
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                "content_filter" => {
                    self.refusal_reason = Some("blocked by the provider's content filter".into());
                    StopReason::Refusal
                }
                _ => StopReason::EndTurn,
            });
        }
    }

    // ---- Gemini ---------------------------------------------------------

    fn push_gemini(&mut self, value: &Value, on_delta: &mut dyn FnMut(Delta)) {
        if let Some(u) = value.get("usageMetadata") {
            self.usage.input_tokens = u
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            self.usage.output_tokens = u
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
        }

        let Some(candidate) = value.pointer("/candidates/0") else {
            return;
        };

        if let Some(parts) = candidate.pointer("/content/parts").and_then(|v| v.as_array()) {
            for part in parts {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    // Gemini marks its reasoning parts rather than using a
                    // separate channel.
                    if part.get("thought").and_then(|v| v.as_bool()).unwrap_or(false) {
                        on_delta(Delta::Thinking(text.to_string()));
                    } else if !text.is_empty() {
                        self.text.push_str(text);
                        on_delta(Delta::Text(text.to_string()));
                    }
                }
                if let Some(call) = part.get("functionCall") {
                    let name = call
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    // A sibling of `functionCall` on the Part, not a field
                    // inside it — which is easy to miss and costs a 400 on the
                    // *next* turn rather than this one.
                    let thought_signature = part
                        .get("thoughtSignature")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if name.is_empty() {
                        continue;
                    }
                    // Gemini does not assign call ids; synthesise stable ones so
                    // results can be matched back on the next turn.
                    let index = self.calls.len();
                    let id = format!("gemini-{index}-{name}");
                    on_delta(Delta::ToolCallStarted {
                        id: id.clone(),
                        name: name.clone(),
                    });
                    self.calls.insert(
                        index,
                        PartialCall {
                            id,
                            name,
                            args_json: call
                                .get("args")
                                .map(|a| a.to_string())
                                .unwrap_or_else(|| "{}".into()),
                            thought_signature,
                        },
                    );
                }
            }
        }

        if let Some(finish) = candidate.get("finishReason").and_then(|v| v.as_str()) {
            self.stop_reason = Some(match finish {
                "MAX_TOKENS" => StopReason::MaxTokens,
                "SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" => {
                    self.refusal_reason = Some(format!("blocked by Gemini safety filters ({finish})"));
                    StopReason::Refusal
                }
                _ if !self.calls.is_empty() => StopReason::ToolUse,
                _ => StopReason::EndTurn,
            });
        }
    }
}

// ---- request bodies -----------------------------------------------------

fn anthropic_body(request: &GenerateRequest) -> Value {
    let mut messages = Vec::new();
    for message in &request.messages {
        match message.role {
            // Anthropic has no top-level system turn inside `messages`.
            Role::System => continue,
            Role::User => messages.push(json!({
                "role": "user",
                "content": [{ "type": "text", "text": message.text }],
            })),
            Role::Assistant => {
                let mut content = Vec::new();
                if !message.text.is_empty() {
                    content.push(json!({ "type": "text", "text": message.text }));
                }
                for call in &message.tool_calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": object_or_empty(&call.arguments),
                    }));
                }
                if content.is_empty() {
                    continue;
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            // Tool results ride in a *user* turn for Anthropic, and every
            // tool_use id must get exactly one matching result.
            Role::Tool => {
                let content: Vec<Value> = message
                    .tool_results
                    .iter()
                    .map(|r| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": r.call_id,
                            "content": r.content,
                            "is_error": r.is_error,
                        })
                    })
                    .collect();
                if !content.is_empty() {
                    messages.push(json!({ "role": "user", "content": content }));
                }
            }
        }
    }

    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "stream": true,
        "messages": messages,
    });
    if !request.system.is_empty() {
        body["system"] = json!([{
            "type": "text",
            "text": request.system,
            // The system prompt and tool list are identical across every turn
            // of a run — caching them is most of the cost of a long agent loop.
            "cache_control": { "type": "ephemeral" },
        }]);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request
            .tools
            .iter()
            .map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            }))
            .collect::<Vec<_>>());
    }
    body
}

/// Coerces a tool call's arguments to a JSON object.
///
/// Every provider validates this field on the way back in and none of them
/// accept `null`, a bare string, or a number — the argument list of a call that
/// took no arguments is `{}`, not "nothing". Applied on the way out rather than
/// only on the way in, because transcripts written by earlier builds are
/// reloaded from SQLite on every single turn and one bad row would poison the
/// project permanently.
fn object_or_empty(value: &Value) -> Value {
    if value.is_object() {
        value.clone()
    } else {
        json!({})
    }
}

fn openai_body(request: &GenerateRequest, provider: Provider) -> Value {
    let mut messages = Vec::new();
    if !request.system.is_empty() {
        messages.push(json!({ "role": "system", "content": request.system }));
    }
    for message in &request.messages {
        match message.role {
            Role::System => messages.push(json!({ "role": "system", "content": message.text })),
            Role::User => messages.push(json!({ "role": "user", "content": message.text })),
            Role::Assistant => {
                let mut entry = json!({ "role": "assistant" });
                entry["content"] = if message.text.is_empty() {
                    Value::Null
                } else {
                    json!(message.text)
                };
                if !message.tool_calls.is_empty() {
                    entry["tool_calls"] = json!(message
                        .tool_calls
                        .iter()
                        .map(|c| json!({
                            "id": c.id,
                            "type": "function",
                            "function": {
                                "name": c.name,
                                "arguments": object_or_empty(&c.arguments).to_string(),
                            },
                        }))
                        .collect::<Vec<_>>());
                }
                messages.push(entry);
            }
            // OpenAI wants one message per tool result, not one grouped turn.
            Role::Tool => {
                for result in &message.tool_results {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": result.call_id,
                        "content": result.content,
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": request.model,
        "stream": true,
        "messages": messages,
    });

    // Only what each one documents, and nothing extra.
    //
    // This is the lesson from the two OpenAI-compatible services that came
    // before: an unrecognised field is not politely ignored by a strict server,
    // it is a 400, and it surfaces as a broken provider rather than a bad
    // parameter. `max_completion_tokens` is OpenAI's spelling and the only one
    // its reasoning models accept; OpenRouter documents `max_tokens`.
    //
    // `stream_options` is an OpenAI extension. OpenRouter does not document it,
    // so it is not sent — the cost is that usage counts arrive as zero there,
    // which is a far smaller loss than a request that never runs.
    match provider {
        Provider::OpenRouter => body["max_tokens"] = json!(request.max_tokens),
        _ => {
            body["max_completion_tokens"] = json!(request.max_tokens);
            body["stream_options"] = json!({ "include_usage": true });
        }
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request
            .tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                },
            }))
            .collect::<Vec<_>>());
    }
    body
}

fn gemini_body(request: &GenerateRequest, include_thoughts: bool) -> Value {
    let mut contents = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::System => continue,
            Role::User => contents.push(json!({
                "role": "user",
                "parts": [{ "text": message.text }],
            })),
            Role::Assistant => {
                let mut parts = Vec::new();
                if !message.text.is_empty() {
                    parts.push(json!({ "text": message.text }));
                }
                for call in &message.tool_calls {
                    let mut part = json!({
                        "functionCall": {
                            "name": call.name,
                            "args": object_or_empty(&call.arguments),
                        },
                    });
                    // Replayed exactly as received. Gemini's thinking models
                    // reject a functionCall that comes back without the
                    // signature they issued with it.
                    if !call.thought_signature.is_empty() {
                        part["thoughtSignature"] = json!(call.thought_signature);
                    }
                    parts.push(part);
                }
                if parts.is_empty() {
                    continue;
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Role::Tool => {
                let parts: Vec<Value> = message
                    .tool_results
                    .iter()
                    .map(|r| {
                        json!({
                            "functionResponse": {
                                "name": r.name,
                                // Gemini requires an object here, so wrap the
                                // tool's plain-text output.
                                "response": { "result": r.content, "is_error": r.is_error },
                            }
                        })
                    })
                    .collect();
                if !parts.is_empty() {
                    contents.push(json!({ "role": "user", "parts": parts }));
                }
            }
        }
    }

    let mut body = json!({
        "contents": contents,
        "generationConfig": { "maxOutputTokens": request.max_tokens },
    });

    // Ask for the reasoning summary.
    //
    // Gemini's thinking models produce no output at all while they think, and
    // they do not stream the thinking unless asked. The result is a request
    // that looks hung for minutes and then answers all at once — which is
    // exactly how a six-minute step reads to someone watching a spinner. With
    // this on, the thought panel fills while the model works, so a long think
    // is visibly a think.
    //
    // Older models reject the field, so a 400 that mentions it retries without.
    if include_thoughts {
        body["generationConfig"]["thinkingConfig"] = json!({ "includeThoughts": true });
    }
    if !request.system.is_empty() {
        body["systemInstruction"] = json!({ "parts": [{ "text": request.system }] });
    }
    if !request.tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": request
                .tools
                .iter()
                .map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": gemini_schema(&t.parameters),
                }))
                .collect::<Vec<_>>(),
        }]);
    }
    body
}

/// Gemini's function-declaration schema is a strict OpenAPI subset: it rejects
/// several JSON Schema keywords that the other two providers accept happily.
fn gemini_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "additionalProperties" | "$schema" | "default" | "examples" | "const"
                ) {
                    continue;
                }
                out.insert(key.clone(), gemini_schema(value));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(gemini_schema).collect()),
        other => other.clone(),
    }
}

pub fn tool_specs_json(tools: &[ToolSpec]) -> Value {
    json!(tools
        .iter()
        .map(|t| json!({ "name": t.name, "description": t.description }))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ToolCall, ToolResult};

    fn request(messages: Vec<ChatMessage>) -> GenerateRequest {
        GenerateRequest {
            provider: Provider::Anthropic,
            model: "claude-opus-5".into(),
            system: "You are VON.".into(),
            messages,
            tools: vec![ToolSpec {
                name: "write_file".into(),
                description: "Write a file".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false,
                }),
            }],
            max_tokens: 4096,
        }
    }

    #[test]
    fn sse_frames_split_on_both_line_endings() {
        assert!(find_frame_end(b"data: {}").is_none());
        let lf = find_frame_end(b"data: {}\n\nnext").unwrap();
        assert_eq!(lf.body_len, 8);
        assert_eq!(lf.end, 10);
        let crlf = find_frame_end(b"data: {}\r\n\r\nnext").unwrap();
        assert_eq!(crlf.body_len, 8);
        assert_eq!(crlf.end, 12);
    }

    #[test]
    fn anthropic_body_sends_no_sampling_parameters() {
        // temperature / top_p / top_k are rejected outright by current models.
        let body = anthropic_body(&request(vec![ChatMessage::user("hi")]));
        for banned in ["temperature", "top_p", "top_k"] {
            assert!(body.get(banned).is_none(), "{banned} must not be sent");
        }
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["system"][0]["cache_control"]["type"], json!("ephemeral"));
    }

    #[test]
    fn anthropic_tool_results_ride_in_a_user_turn() {
        let messages = vec![
            ChatMessage::user("write a file"),
            ChatMessage::assistant(
                "",
                vec![ToolCall {
                    id: "toolu_1".into(),
                    name: "write_file".into(),
                    arguments: json!({ "path": "a.py" }),
                    thought_signature: String::new(),
                }],
            ),
            ChatMessage::tool(vec![ToolResult {
                call_id: "toolu_1".into(),
                name: "write_file".into(),
                content: "ok".into(),
                is_error: false,
            }]),
        ];
        let body = anthropic_body(&request(messages));
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns[1]["role"], json!("assistant"));
        assert_eq!(turns[1]["content"][0]["type"], json!("tool_use"));
        assert_eq!(turns[2]["role"], json!("user"));
        assert_eq!(turns[2]["content"][0]["type"], json!("tool_result"));
        assert_eq!(turns[2]["content"][0]["tool_use_id"], json!("toolu_1"));
    }

    #[test]
    fn openai_emits_one_message_per_tool_result() {
        let mut req = request(vec![ChatMessage::tool(vec![
            ToolResult {
                call_id: "a".into(),
                name: "write_file".into(),
                content: "ok".into(),
                is_error: false,
            },
            ToolResult {
                call_id: "b".into(),
                name: "read_file".into(),
                content: "data".into(),
                is_error: false,
            },
        ])]);
        req.provider = Provider::OpenAi;
        let body = openai_body(&req, Provider::OpenAi);
        let turns = body["messages"].as_array().unwrap();
        // system + two tool messages
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1]["tool_call_id"], json!("a"));
        assert_eq!(turns[2]["tool_call_id"], json!("b"));
    }

    #[test]
    fn a_null_argument_list_never_reaches_a_provider() {
        assert_eq!(object_or_empty(&Value::Null), json!({}));
        assert_eq!(object_or_empty(&json!("a string")), json!({}));
        assert_eq!(object_or_empty(&json!(42)), json!({}));
        assert_eq!(object_or_empty(&json!([1, 2])), json!({}));
        // A real argument list is passed through untouched.
        assert_eq!(object_or_empty(&json!({"path": "a.py"})), json!({"path": "a.py"}));
    }

    #[test]
    fn a_streamed_null_becomes_an_empty_object() {
        let mut acc = Accumulator::default();
        acc.push_openai(
            &json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"call_1","function":{"name":"list_files","arguments":"null"}}
            ]}}]}),
            &mut |_| {},
        );
        let done = acc.finish();
        assert_eq!(done.tool_calls.len(), 1);
        assert!(
            done.tool_calls[0].arguments.is_object(),
            "got {:?}",
            done.tool_calls[0].arguments
        );
    }

    /// Gemini 3's thinking models sign each function call and refuse the next
    /// turn if the signature does not come back with it — "Function call is
    /// missing a thought_signature in functionCall parts". The signature sits
    /// on the Part, beside functionCall rather than inside it, which is easy to
    /// miss and costs a 400 on the turn *after* the one that dropped it.
    #[test]
    fn a_gemini_thought_signature_survives_the_round_trip() {
        let mut acc = Accumulator::default();
        acc.push_gemini(
            &json!({"candidates":[{"content":{"parts":[{
                "functionCall": {"name": "list_files", "args": {}},
                "thoughtSignature": "Cs4BAVSoXO8-signature"
            }]}}]}),
            &mut |_| {},
        );
        let done = acc.finish();
        assert_eq!(done.tool_calls.len(), 1);
        assert_eq!(done.tool_calls[0].thought_signature, "Cs4BAVSoXO8-signature");

        // And it goes back out on the model turn.
        let replayed = gemini_body(
            &request(vec![ChatMessage::assistant("", done.tool_calls.clone())]),
            true,
        );
        let part = &replayed["contents"][0]["parts"][0];
        assert_eq!(part["thoughtSignature"], json!("Cs4BAVSoXO8-signature"));
        assert_eq!(part["functionCall"]["name"], json!("list_files"));
    }

    /// Every other provider leaves it empty, and an empty signature must not be
    /// sent — Gemini rejects a blank one as readily as a missing one.
    #[test]
    fn an_unsigned_call_sends_no_signature_field() {
        let call = ToolCall {
            id: "1".into(),
            name: "list_files".into(),
            arguments: json!({}),
            thought_signature: String::new(),
        };
        let body = gemini_body(&request(vec![ChatMessage::assistant("", vec![call])]), true);
        let part = &body["contents"][0]["parts"][0];
        assert!(part.get("thoughtSignature").is_none());
    }

    /// Without this, a thinking model produces no bytes at all while it
    /// reasons and then answers at once — which is indistinguishable from a
    /// hung request for however long the thinking takes.
    #[test]
    fn gemini_is_asked_for_its_reasoning() {
        let req = request(vec![ChatMessage::user("hi")]);

        let thinking = gemini_body(&req, true);
        assert_eq!(
            thinking["generationConfig"]["thinkingConfig"]["includeThoughts"],
            json!(true)
        );

        // And the fallback for a model that rejects the field sends no trace
        // of it, rather than sending it set to false.
        let plain = gemini_body(&req, false);
        assert!(plain["generationConfig"].get("thinkingConfig").is_none());
        assert_eq!(
            plain["generationConfig"]["maxOutputTokens"],
            json!(req.max_tokens)
        );
    }

    #[test]
    fn gemini_schema_drops_unsupported_keywords() {
        let cleaned = gemini_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "x": { "type": "string", "default": "y" } },
        }));
        assert!(cleaned.get("additionalProperties").is_none());
        assert!(cleaned["properties"]["x"].get("default").is_none());
        assert_eq!(cleaned["properties"]["x"]["type"], json!("string"));
    }

    #[test]
    fn anthropic_stream_reduces_to_a_tool_call() {
        let mut acc = Accumulator::default();
        let mut deltas = Vec::new();
        let mut sink = |d: Delta| deltas.push(d);

        for event in [
            json!({"type":"message_start","message":{"usage":{"input_tokens":42}}}),
            json!({"type":"content_block_start","index":0,
                   "content_block":{"type":"tool_use","id":"toolu_9","name":"write_file"}}),
            json!({"type":"content_block_delta","index":0,
                   "delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            json!({"type":"content_block_delta","index":0,
                   "delta":{"type":"input_json_delta","partial_json":"\"main.py\"}"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
                   "usage":{"output_tokens":17}}),
        ] {
            acc.push_anthropic(&event, &mut sink).unwrap();
        }

        let done = acc.finish();
        assert_eq!(done.stop_reason, StopReason::ToolUse);
        assert_eq!(done.usage.input_tokens, 42);
        assert_eq!(done.usage.output_tokens, 17);
        assert_eq!(done.tool_calls.len(), 1);
        assert_eq!(done.tool_calls[0].name, "write_file");
        assert_eq!(done.tool_calls[0].arguments["path"], json!("main.py"));
    }

    #[test]
    fn anthropic_refusal_is_surfaced_not_treated_as_a_normal_turn() {
        let mut acc = Accumulator::default();
        acc.push_anthropic(
            &json!({"type":"message_delta",
                    "delta":{"stop_reason":"refusal","stop_details":{"category":"cyber"}}}),
            &mut |_| {},
        )
        .unwrap();
        let done = acc.finish();
        assert_eq!(done.stop_reason, StopReason::Refusal);
        assert!(done.refusal_reason.unwrap().contains("cyber"));
    }

    #[test]
    fn openai_stream_assembles_split_tool_arguments() {
        let mut acc = Accumulator::default();
        for event in [
            json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"th\":\"x.py\"}"}}]}}]}),
            json!({"choices":[{"finish_reason":"tool_calls","delta":{}}],
                   "usage":{"prompt_tokens":10,"completion_tokens":3}}),
        ] {
            acc.push_openai(&event, &mut |_| {});
        }
        let done = acc.finish();
        assert_eq!(done.stop_reason, StopReason::ToolUse);
        assert_eq!(done.tool_calls[0].arguments["path"], json!("x.py"));
    }

    #[test]
    fn truncated_tool_arguments_degrade_to_an_empty_object() {
        let mut acc = Accumulator::default();
        acc.calls.insert(
            0,
            PartialCall {
                id: "x".into(),
                name: "write_file".into(),
                args_json: "{\"path\": \"unter".into(),
                ..Default::default()
            },
        );
        // The tool then reports a clean "missing argument" the agent can fix,
        // instead of the stream killing the whole run.
        assert_eq!(acc.finish().tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn provider_error_bodies_are_summarised() {
        let msg = summarise_error(r#"{"error":{"message":"invalid x-api-key"}}"#);
        assert_eq!(msg, "invalid x-api-key");
        assert!(summarise_error(&"z".repeat(5000)).len() <= 400);
    }
}
