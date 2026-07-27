//! Driving the agent from Dart (PRD Process B).

use flutter_rust_bridge::StreamSink;

use crate::agent::{self, AgentEvent, RunOptions};
use crate::api::types::{ApiResult, VonException};
use crate::error::ErrorInfo;

/// Mirror of [`AgentEvent`] flattened for the bridge.
///
/// One struct with a `kind` discriminator rather than a Dart sealed class:
/// generated union types across FFI are awkward to pattern-match in Riverpod
/// notifiers, and every field here is cheap to carry.
#[derive(Debug, Clone)]
pub struct AgentEventDto {
    /// `phase` | `token` | `thought` | `tool_started` | `tool_finished` |
    /// `files_changed` | `preview_ready` | `message_saved` | `throttled` |
    /// `done` | `failed`
    pub kind: String,
    pub text: String,
    pub detail: String,
    pub call_id: String,
    pub tool_name: String,
    pub ok: bool,
    pub seq: i64,
    pub role: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub steps: i64,
    pub error_kind: String,
    pub error_transient: bool,
}

impl Default for AgentEventDto {
    fn default() -> Self {
        Self {
            kind: String::new(),
            text: String::new(),
            detail: String::new(),
            call_id: String::new(),
            tool_name: String::new(),
            ok: true,
            seq: -1,
            role: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            steps: 0,
            error_kind: String::new(),
            error_transient: false,
        }
    }
}

impl From<AgentEvent> for AgentEventDto {
    fn from(event: AgentEvent) -> Self {
        match event {
            AgentEvent::Phase { phase, detail } => Self {
                kind: "phase".into(),
                text: phase,
                detail,
                ..Default::default()
            },
            AgentEvent::Token { text } => Self {
                kind: "token".into(),
                text,
                ..Default::default()
            },
            AgentEvent::Thought { text } => Self {
                kind: "thought".into(),
                text,
                ..Default::default()
            },
            AgentEvent::ToolStarted {
                call_id,
                name,
                summary,
            } => Self {
                kind: "tool_started".into(),
                call_id,
                tool_name: name,
                detail: summary,
                ..Default::default()
            },
            AgentEvent::ToolFinished {
                call_id,
                name,
                ok,
                output,
            } => Self {
                kind: "tool_finished".into(),
                call_id,
                tool_name: name,
                ok,
                text: output,
                ..Default::default()
            },
            AgentEvent::FilesChanged => Self {
                kind: "files_changed".into(),
                ..Default::default()
            },
            AgentEvent::PreviewReady { url } => Self {
                kind: "preview_ready".into(),
                text: url,
                ..Default::default()
            },
            AgentEvent::MessageSaved { seq, role, content } => Self {
                kind: "message_saved".into(),
                seq,
                role,
                text: content,
                ..Default::default()
            },
            AgentEvent::Throttled { state, message } => Self {
                kind: "throttled".into(),
                text: state,
                detail: message,
                ..Default::default()
            },
            AgentEvent::Done {
                stop_reason,
                summary,
                input_tokens,
                output_tokens,
                steps,
            } => Self {
                kind: "done".into(),
                text: stop_reason,
                detail: summary,
                input_tokens: input_tokens as i64,
                output_tokens: output_tokens as i64,
                steps: steps as i64,
                ..Default::default()
            },
            AgentEvent::Failed { error } => Self::from_error(&error),
        }
    }
}

impl AgentEventDto {
    fn from_error(error: &ErrorInfo) -> Self {
        Self {
            kind: "failed".into(),
            text: error.message.clone(),
            error_kind: error.kind.clone(),
            error_transient: error.transient,
            ok: false,
            ..Default::default()
        }
    }
}

/// Runs one user turn, streaming everything the UI needs.
///
/// The stream always terminates with a `done` or `failed` event, so the Dart
/// side can rely on a single completion signal.
pub fn run_agent(
    sink: StreamSink<AgentEventDto>,
    project_id: String,
    message: String,
    max_tokens: u32,
) -> ApiResult<()> {
    let message = message.trim().to_string();
    if message.is_empty() {
        return Err(crate::error::VonError::InvalidArgument("write a message first".into()).into());
    }

    let options = RunOptions {
        project_id,
        user_message: message,
        // Clamped: an unbounded value would let one turn burn the whole
        // context window (and, on a cloud model, real money).
        max_tokens: max_tokens.clamp(256, 32_000),
    };

    let mut closed = false;
    let result = agent::run(options, &mut |event| {
        if closed {
            return;
        }
        if sink.add(AgentEventDto::from(event)).is_err() {
            // Dart dropped the subscription. Stop pushing, but let the run
            // finish so the transcript and files stay consistent.
            closed = true;
        }
    });

    result.map_err(VonException::from)
}

/// Stops the run in progress. The loop finishes the current tool call, records
/// results for anything already started, and exits cleanly.
pub fn cancel_agent(project_id: String) -> ApiResult<()> {
    agent::cancel(&project_id);
    Ok(())
}
