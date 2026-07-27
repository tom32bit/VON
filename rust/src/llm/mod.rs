//! Model abstraction: one transcript format, several backends.
//!
//! The agent loop is written once against [`Engine`]. Cloud providers get their
//! native tool-calling wire format (translated in `cloud`), and local GGUF
//! models get a text protocol the same loop understands (`local`), so a project
//! can move between them mid-conversation without rewriting its history.

pub mod catalog;
pub mod cloud;
pub mod discovery;
pub mod local;

use serde::{Deserialize, Serialize};

use crate::error::{Result, VonError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Local,
    Anthropic,
    OpenAi,
    Gemini,
    /// A normalising proxy in front of everyone else's models.
    ///
    /// Unlike the OpenAI-shaped services VON has carried before, OpenRouter's
    /// whole job is to present one schema over many backends — it is compatible
    /// by design rather than by imitation, which is a different risk profile.
    OpenRouter,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::OpenRouter => "openrouter",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "local" | "offline" | "gguf" => Ok(Self::Local),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "openai" | "gpt" => Ok(Self::OpenAi),
            "gemini" | "google" => Ok(Self::Gemini),
            "openrouter" | "open-router" | "or" => Ok(Self::OpenRouter),
            // Named rather than lumped in with a typo: a VON created while
            // these were offered still has one recorded, and "unknown provider
            // 'groq'" would read as a bug rather than a removal.
            removed @ ("groq" | "nvidia" | "nim" | "nvidia-nim") => {
                Err(VonError::InvalidArgument(format!(
                    "'{removed}' is no longer one of VON's providers. Use Switch Model to pick \
                     another for this VON — the conversation and files are untouched."
                )))
            }
            other => Err(VonError::InvalidArgument(format!(
                "unknown provider '{other}'"
            ))),
        }
    }

    pub fn is_cloud(self) -> bool {
        !matches!(self, Self::Local)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Local => "On-Device",
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Google Gemini",
            Self::OpenRouter => "OpenRouter",
        }
    }

    /// Every provider VON offers, for the settings and picker screens.
    pub const ALL_CLOUD: [Self; 4] = [
        Self::Anthropic,
        Self::OpenAi,
        Self::Gemini,
        Self::OpenRouter,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "system" => Self::System,
            "assistant" => Self::Assistant,
            "tool" => Self::Tool,
            _ => Self::User,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    /// Gemini's opaque proof that this call came out of the model's own
    /// reasoning.
    ///
    /// Thinking models sign each function call and require the signature back
    /// with it on the next turn; without it the request is refused outright.
    /// Empty for every other provider, and `serde(default)` so transcripts
    /// written before this field existed still load.
    #[serde(default)]
    pub thought_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

/// One turn of the normalised transcript. An assistant turn carries `text` plus
/// any `tool_calls`; a tool turn carries only `tool_results`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_results: Vec<ToolResult>,
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            tool_calls,
            tool_results: Vec::new(),
        }
    }

    pub fn tool(results: Vec<ToolResult>) -> Self {
        Self {
            role: Role::Tool,
            text: String::new(),
            tool_calls: Vec::new(),
            tool_results: results,
        }
    }

    /// Rough token estimate used for context-window shifting. Deliberately
    /// cheap — we shift on a trend, not on a precise count.
    pub fn approx_tokens(&self) -> usize {
        let mut chars = self.text.len();
        for call in &self.tool_calls {
            chars += call.name.len() + call.arguments.to_string().len();
        }
        for result in &self.tool_results {
            chars += result.content.len();
        }
        // ~4 characters per token holds well enough for English and code.
        (chars / 4) + 4
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// The model finished its turn normally.
    EndTurn,
    /// The model wants one or more tools run.
    ToolUse,
    /// Output token cap reached.
    MaxTokens,
    /// The provider's safety classifiers declined the request.
    Refusal,
    /// The user pressed Stop, or the governor halted generation.
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Streamed increments. The UI renders `Text` token-by-token and shows
/// `Thinking` in the agent's thought-process panel.
#[derive(Debug, Clone)]
pub enum Delta {
    Text(String),
    Thinking(String),
    ToolCallStarted { id: String, name: String },
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: StopReason,
    pub usage: Usage,
    /// Populated when `stop_reason` is `Refusal`.
    pub refusal_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GenerateRequest {
    pub provider: Provider,
    pub model: String,
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
}

/// A backend that can produce one assistant turn.
///
/// Blocking by design: the agent loop already runs on a background thread, and
/// a blocking interface keeps the local (`llama.cpp`) implementation — which is
/// inherently synchronous — from having to fake being async.
pub trait Engine: Send {
    fn generate(
        &self,
        request: &GenerateRequest,
        on_delta: &mut dyn FnMut(Delta),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Completion>;
}

/// Builds the engine for a request, resolving API keys from the database.
/// Builds the engine for a project.
///
/// `model_id` is the project's own choice, which for the local path is not the
/// same thing as the Model Hub's default. Selecting a model for one VON does
/// not — and should not — change what a new VON starts with, so the engine has
/// to be told which weights this project means rather than reading the global
/// setting behind its back.
pub fn engine_for(provider: Provider, model_id: &str) -> Result<Box<dyn Engine>> {
    match provider {
        Provider::Local => Ok(Box::new(local::LocalEngine::for_model(model_id)?)),
        cloud_provider => {
            let key = crate::db::get_api_key(cloud_provider.as_str())?
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| VonError::MissingApiKey(cloud_provider.as_str().to_string()))?;
            Ok(Box::new(cloud::CloudEngine::new(cloud_provider, key)))
        }
    }
}

/// Trims a transcript to fit a token budget (SWE §8, "context window shifting").
///
/// Keeps the most recent turns and replaces what it drops with a short marker,
/// so the model knows history was elided rather than silently inventing what it
/// can no longer see. The first user message is always kept — it is usually the
/// task statement, and losing it is how an agent forgets what it is doing.
pub fn shift_context(messages: &[ChatMessage], budget_tokens: usize) -> Vec<ChatMessage> {
    let total: usize = messages.iter().map(|m| m.approx_tokens()).sum();
    if total <= budget_tokens || messages.len() <= 3 {
        return messages.to_vec();
    }

    let mut kept: Vec<ChatMessage> = Vec::new();
    let mut used = 0usize;

    // Walk backwards, keeping recent turns until the budget is spent.
    for message in messages.iter().rev() {
        let cost = message.approx_tokens();
        if used + cost > budget_tokens && kept.len() >= 2 {
            break;
        }
        used += cost;
        kept.push(message.clone());
    }
    kept.reverse();

    // A tool turn whose matching assistant turn was dropped is malformed for
    // every provider — drop the orphans from the front.
    while kept
        .first()
        .map(|m| m.role == Role::Tool)
        .unwrap_or(false)
    {
        kept.remove(0);
    }

    let dropped = messages.len() - kept.len();
    if dropped == 0 {
        return kept;
    }

    let mut out = Vec::with_capacity(kept.len() + 2);
    if let Some(first_user) = messages.iter().find(|m| m.role == Role::User) {
        out.push(first_user.clone());
    }
    out.push(ChatMessage::user(format!(
        "[{dropped} earlier messages were dropped to fit the context window. \
         The files you already created are still in the sandbox — call \
         list_files if you need to re-orient.]"
    )));
    out.extend(kept);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_parsing_round_trips() {
        for p in [
            Provider::Local,
            Provider::Anthropic,
            Provider::OpenAi,
            Provider::Gemini,
        ] {
            assert_eq!(Provider::parse(p.as_str()).unwrap(), p);
        }
        assert!(Provider::parse("cohere").is_err());
    }

    #[test]
    fn short_transcripts_pass_through_untouched() {
        let messages = vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello", vec![]),
        ];
        assert_eq!(shift_context(&messages, 10_000).len(), 2);
    }

    #[test]
    fn shifting_keeps_the_task_statement_and_recent_turns() {
        let mut messages = vec![ChatMessage::user("Build the reporting tool")];
        for i in 0..60 {
            messages.push(ChatMessage::assistant(format!("{} step {i}", "x".repeat(400)), vec![]));
            messages.push(ChatMessage::user(format!("continue {i}")));
        }
        let shifted = shift_context(&messages, 2_000);

        assert!(shifted.len() < messages.len());
        assert_eq!(shifted[0].text, "Build the reporting tool");
        assert!(shifted[1].text.contains("dropped to fit the context window"));
        // The most recent turn always survives.
        assert_eq!(
            shifted.last().unwrap().text,
            messages.last().unwrap().text
        );
    }

    #[test]
    fn shifting_never_leaves_an_orphaned_tool_turn_first() {
        let mut messages = vec![ChatMessage::user("start")];
        for i in 0..40 {
            messages.push(ChatMessage::assistant(
                "x".repeat(600),
                vec![ToolCall {
                    id: format!("t{i}"),
                    name: "write_file".into(),
                    arguments: serde_json::json!({}),
                    thought_signature: String::new(),
                }],
            ));
            messages.push(ChatMessage::tool(vec![ToolResult {
                call_id: format!("t{i}"),
                name: "write_file".into(),
                content: "ok".into(),
                is_error: false,
            }]));
        }
        let shifted = shift_context(&messages, 1_500);
        // Index 0 is the preserved task statement, 1 the elision marker; the
        // first real turn after that must not be a dangling tool result.
        assert_ne!(shifted[2].role, Role::Tool);
    }
}
