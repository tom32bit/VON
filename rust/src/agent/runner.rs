//! The agent execution loop (PRD Process B).
//!
//! Plan → call tools → observe → repeat, until the agent says it's done, the
//! user stops it, or a guard trips. Everything the UI shows during a run is an
//! [`AgentEvent`] emitted from here.
//!
//! Two rules shape the design:
//!
//! * **Persist as you go.** Android can kill the process between any two steps,
//!   so each completed turn is written to SQLite immediately — not batched to
//!   the end of the run.
//! * **Never strand a tool call.** Every provider rejects an assistant turn with
//!   a `tool_use` that has no matching result. On cancellation and on error the
//!   loop still records results for calls it started.

use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};

use crate::agent::{prompt, tools};
use crate::db::{self, MessageRow};
use crate::error::{ErrorInfo, Result, VonError};
use crate::governor;
use crate::llm::{
    self,
    ChatMessage,
    Delta,
    GenerateRequest,
    Provider,
    Role,
    StopReason,
    ToolResult,
};
use crate::state;

/// Hard ceiling on tool-calling turns in a single run. A model stuck in a
/// read/write cycle would otherwise drain the battery unattended.
const MAX_STEPS: usize = 24;
/// Transcript budget before context shifting kicks in (SWE §8).
const CONTEXT_BUDGET_TOKENS: usize = 24_000;
/// Consecutive transient provider failures tolerated before giving up.
const MAX_RETRIES: usize = 3;

/// What the UI renders. Mirrored into Dart by `flutter_rust_bridge`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    /// Coarse phase for the thought-process header: `planning`, `calling_tool`,
    /// `executing`, `observing`, `retrying`.
    Phase { phase: String, detail: String },
    /// A token of the assistant's visible reply.
    Token { text: String },
    /// A token of the model's reasoning summary, shown in the thought panel.
    Thought { text: String },
    ToolStarted {
        call_id: String,
        name: String,
        summary: String,
    },
    ToolFinished {
        call_id: String,
        name: String,
        ok: bool,
        output: String,
    },
    /// The Sandbox Explorer should refresh.
    FilesChanged,
    PreviewReady { url: String },
    /// A full assistant turn was persisted; `seq` matches the SQLite row.
    MessageSaved { seq: i64, role: String, content: String },
    /// The device state changed enough that the UI should say so.
    Throttled { state: String, message: String },
    Done {
        stop_reason: String,
        summary: String,
        input_tokens: u32,
        output_tokens: u32,
        steps: u32,
    },
    Failed { error: ErrorInfo },
}

pub struct RunOptions {
    pub project_id: String,
    pub user_message: String,
    pub max_tokens: u32,
}

/// Runs one user turn to completion, emitting events as it goes.
///
/// `emit` is called from this thread only, so the caller does not need to
/// synchronise. Returns `Ok(())` even when the run ends in a handled failure —
/// the failure is reported as an `AgentEvent::Failed`, because a run that hit a
/// provider error is not a bug in the bridge.
pub fn run(options: RunOptions, emit: &mut dyn FnMut(AgentEvent)) -> Result<()> {
    match run_inner(&options, emit) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!("agent run failed: {e}");
            emit(AgentEvent::Failed {
                error: ErrorInfo::from(&e),
            });
            Ok(())
        }
    }
}

fn run_inner(options: &RunOptions, emit: &mut dyn FnMut(AgentEvent)) -> Result<()> {
    let project_id = options.project_id.as_str();
    let project = db::get_project(project_id)?;
    let sandbox = state::sandbox_for(project_id)?;
    let cancel = state::begin_run(project_id);
    let cancelled = || cancel.load(Ordering::SeqCst);

    let provider = llm::Provider::parse(&project.provider)?;
    let engine = llm::engine_for(provider)?;
    let tool_specs = tools::specs(&project.kind);

    // Restore the transcript so the agent resumes with full context (Process C).
    let mut messages = load_transcript(project_id)?;
    messages.push(ChatMessage::user(options.user_message.clone()));
    persist_message(
        project_id,
        Role::User,
        &options.user_message,
        "[]",
        emit,
    )?;

    let device = db::load_device_profile()?.map(|row| governor::DeviceProfile {
        tier: governor::DeviceTier::from_i32(row.tier),
        cores: row.cores as u32,
        total_ram_mb: row.total_ram_mb as u64,
        available_ram_mb: 0,
        score: row.score,
        max_local_params_b: 0.0,
        notes: Vec::new(),
    });
    let memory = db::load_memory(project_id)?;

    let mut totals = llm::Usage::default();
    let mut steps: u32 = 0;
    let mut retries = 0usize;
    let mut final_summary = String::new();
    let mut stop_label = "end_turn";

    for step in 0..MAX_STEPS {
        if cancelled() {
            stop_label = "cancelled";
            break;
        }

        // Rebuild the system prompt each turn: the file listing is part of it,
        // and it changes as the agent works.
        let system = prompt::system_prompt(&prompt::PromptContext {
            project_name: &project.name,
            project_ordinal: project.ordinal,
            project_kind: &project.kind,
            provider,
            existing_files: &sandbox.list_flat().unwrap_or_default(),
            device: device.as_ref(),
            memory_summary: &memory.summary,
        });

        let trimmed = llm::shift_context(&messages, CONTEXT_BUDGET_TOKENS);

        emit(AgentEvent::Phase {
            phase: "planning".into(),
            detail: if step == 0 {
                "Reading the request".into()
            } else {
                format!("Deciding what to do next (step {})", step + 1)
            },
        });

        // Warn once per turn when the device is working against us.
        let stats = governor::stats();
        if stats.thermal >= governor::ThermalState::Light && provider == Provider::Local {
            emit(AgentEvent::Throttled {
                state: stats.thermal.as_str().to_string(),
                message: format!(
                    "Device at {:.0}°C — generating more slowly to let it cool.",
                    stats.temperature_c
                ),
            });
        }

        let request = GenerateRequest {
            provider,
            model: project.model_id.clone(),
            system,
            messages: trimmed,
            tools: tool_specs.clone(),
            max_tokens: options.max_tokens,
        };

        let completion = match engine.generate(
            &request,
            &mut |delta| match delta {
                Delta::Text(text) => emit(AgentEvent::Token { text }),
                Delta::Thinking(text) => emit(AgentEvent::Thought { text }),
                Delta::ToolCallStarted { id, name } => emit(AgentEvent::ToolStarted {
                    call_id: id,
                    summary: format!("Preparing {name}"),
                    name,
                }),
            },
            &cancelled,
        ) {
            Ok(c) => c,
            Err(e) if e.is_transient() && retries < MAX_RETRIES && !cancelled() => {
                retries += 1;
                let backoff = std::time::Duration::from_millis(500 * (1 << retries) as u64);
                emit(AgentEvent::Phase {
                    phase: "retrying".into(),
                    detail: format!("{e}. Retrying in {}s…", backoff.as_secs_f32()),
                });
                std::thread::sleep(backoff);
                continue;
            }
            Err(e) => return Err(e),
        };
        retries = 0;

        totals.input_tokens += completion.usage.input_tokens;
        totals.output_tokens += completion.usage.output_tokens;
        steps += 1;

        if completion.stop_reason == StopReason::Refusal {
            let reason = completion
                .refusal_reason
                .unwrap_or_else(|| "the provider declined this request".into());
            persist_message(project_id, Role::Assistant, &reason, "[]", emit)?;
            emit(AgentEvent::Done {
                stop_reason: "refusal".into(),
                summary: reason,
                input_tokens: totals.input_tokens,
                output_tokens: totals.output_tokens,
                steps,
            });
            return Ok(());
        }

        // Record the assistant turn — including its tool calls — before running
        // anything, so a crash mid-tool leaves a coherent transcript.
        let assistant = ChatMessage::assistant(completion.text.clone(), completion.tool_calls.clone());
        let steps_json = serde_json::to_string(&assistant.tool_calls).unwrap_or_else(|_| "[]".into());
        if !assistant.text.is_empty() || !assistant.tool_calls.is_empty() {
            persist_message(project_id, Role::Assistant, &assistant.text, &steps_json, emit)?;
        }
        messages.push(assistant);

        if completion.tool_calls.is_empty() {
            final_summary = completion.text;
            stop_label = match completion.stop_reason {
                StopReason::MaxTokens => "max_tokens",
                StopReason::Cancelled => "cancelled",
                _ => "end_turn",
            };
            break;
        }

        // ---- run the tools ----
        emit(AgentEvent::Phase {
            phase: "executing".into(),
            detail: format!("{} tool call(s)", completion.tool_calls.len()),
        });

        let mut results: Vec<ToolResult> = Vec::with_capacity(completion.tool_calls.len());
        let mut completed: Option<String> = None;

        for call in &completion.tool_calls {
            if cancelled() {
                // Still emit a result: an unanswered tool_use makes the whole
                // transcript unusable on the next turn.
                results.push(ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: "Cancelled by the user before this tool ran.".into(),
                    is_error: true,
                });
                continue;
            }

            let summary = tools::describe(call);
            emit(AgentEvent::ToolStarted {
                call_id: call.id.clone(),
                name: call.name.clone(),
                summary: summary.clone(),
            });

            let execution = tools::execute(&sandbox, project_id, call);

            emit(AgentEvent::ToolFinished {
                call_id: call.id.clone(),
                name: call.name.clone(),
                ok: !execution.result.is_error,
                output: execution.result.content.clone(),
            });
            if execution.effects.files_changed {
                emit(AgentEvent::FilesChanged);
            }
            if let Some(url) = execution.effects.preview_url {
                emit(AgentEvent::PreviewReady { url });
            }
            if let Some(summary) = execution.effects.completed {
                completed = Some(summary);
            }
            results.push(execution.result);
        }

        let results_json = serde_json::to_string(&results).unwrap_or_else(|_| "[]".into());
        persist_message(project_id, Role::Tool, "", &results_json, emit)?;
        messages.push(ChatMessage::tool(results));

        emit(AgentEvent::Phase {
            phase: "observing".into(),
            detail: "Reading the results".into(),
        });

        if let Some(summary) = completed {
            final_summary = if summary.is_empty() {
                completion.text.clone()
            } else {
                summary
            };
            stop_label = "completed";
            break;
        }

        if cancelled() {
            stop_label = "cancelled";
            break;
        }

        if step + 1 == MAX_STEPS {
            stop_label = "step_limit";
            final_summary = format!(
                "Stopped after {MAX_STEPS} steps to protect the battery. Send another message \
                 to continue from here."
            );
        }
    }

    // Snapshot for the next session (Process C: Resumption).
    save_memory(project_id, &messages)?;
    let usage = sandbox.usage()?;
    db::touch_project(project_id, usage.total_bytes as i64)?;

    emit(AgentEvent::Done {
        stop_reason: stop_label.to_string(),
        summary: final_summary,
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        steps,
    });
    Ok(())
}

// ---- persistence --------------------------------------------------------

fn persist_message(
    project_id: &str,
    role: Role,
    content: &str,
    steps_json: &str,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<()> {
    let seq = db::next_seq(project_id)?;
    db::insert_message(&MessageRow {
        id: uuid::Uuid::new_v4().to_string(),
        project_id: project_id.to_string(),
        seq,
        role: role.as_str().to_string(),
        content: content.to_string(),
        steps_json: steps_json.to_string(),
        created_at: chrono::Utc::now().timestamp_millis(),
    })?;
    emit(AgentEvent::MessageSaved {
        seq,
        role: role.as_str().to_string(),
        content: content.to_string(),
    });
    Ok(())
}

/// Rebuilds the in-memory transcript from SQLite.
fn load_transcript(project_id: &str) -> Result<Vec<ChatMessage>> {
    let rows = db::list_messages(project_id)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let role = Role::parse(&row.role);
        let message = match role {
            Role::Assistant => ChatMessage::assistant(
                row.content,
                serde_json::from_str(&row.steps_json).unwrap_or_default(),
            ),
            Role::Tool => ChatMessage::tool(
                serde_json::from_str(&row.steps_json).unwrap_or_default(),
            ),
            _ => ChatMessage {
                role,
                text: row.content,
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
            },
        };
        out.push(message);
    }
    Ok(out)
}

fn save_memory(project_id: &str, messages: &[ChatMessage]) -> Result<()> {
    let token_estimate: usize = messages.iter().map(|m| m.approx_tokens()).sum();
    db::save_memory(
        project_id,
        &db::MemoryRow {
            context_json: serde_json::to_string(messages).unwrap_or_else(|_| "[]".into()),
            summary: rolling_summary(messages),
            token_estimate: token_estimate as i64,
        },
    )
}

/// A cheap extractive summary of what the agent has done so far.
///
/// Deliberately not model-generated: summarising with the model would cost an
/// extra request at the end of every run, and the value here is a factual list
/// of actions, which the transcript already contains verbatim.
fn rolling_summary(messages: &[ChatMessage]) -> String {
    let mut written: Vec<&str> = Vec::new();
    let mut ran = 0usize;
    for message in messages {
        for call in &message.tool_calls {
            match call.name.as_str() {
                "write_file" => {
                    if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                        if !written.contains(&path) {
                            written.push(path);
                        }
                    }
                }
                "run_python" | "run_wasm" => ran += 1,
                _ => {}
            }
        }
    }
    if written.is_empty() && ran == 0 {
        return String::new();
    }
    let mut out = String::new();
    if !written.is_empty() {
        out.push_str(&format!("Created: {}. ", written.join(", ")));
    }
    if ran > 0 {
        out.push_str(&format!("Ran {ran} program(s)."));
    }
    out
}

/// Stops the run for a project. Safe to call when nothing is running.
pub fn cancel(project_id: &str) {
    state::request_cancel(project_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolCall;
    use serde_json::json;

    fn assistant_with(name: &str, args: serde_json::Value) -> ChatMessage {
        ChatMessage::assistant(
            "",
            vec![ToolCall {
                id: "1".into(),
                name: name.into(),
                arguments: args,
            }],
        )
    }

    #[test]
    fn summary_lists_files_and_counts_runs() {
        let messages = vec![
            ChatMessage::user("build it"),
            assistant_with("write_file", json!({"path": "report.py"})),
            assistant_with("write_file", json!({"path": "chart.py"})),
            assistant_with("run_python", json!({"path": "report.py"})),
        ];
        let summary = rolling_summary(&messages);
        assert!(summary.contains("report.py"));
        assert!(summary.contains("chart.py"));
        assert!(summary.contains("Ran 1 program"));
    }

    #[test]
    fn summary_deduplicates_repeated_writes() {
        let messages = vec![
            assistant_with("write_file", json!({"path": "a.py"})),
            assistant_with("write_file", json!({"path": "a.py"})),
        ];
        assert_eq!(rolling_summary(&messages).matches("a.py").count(), 1);
    }

    #[test]
    fn summary_is_empty_before_any_work() {
        assert!(rolling_summary(&[ChatMessage::user("hi")]).is_empty());
    }

    #[test]
    fn agent_events_round_trip_as_json() {
        // The bridge mirrors these into Dart; a serialisation break here is a
        // silently broken UI.
        let events = vec![
            AgentEvent::Token { text: "hi".into() },
            AgentEvent::ToolFinished {
                call_id: "1".into(),
                name: "write_file".into(),
                ok: true,
                output: "wrote".into(),
            },
            AgentEvent::Done {
                stop_reason: "completed".into(),
                summary: "done".into(),
                input_tokens: 10,
                output_tokens: 20,
                steps: 3,
            },
        ];
        for event in events {
            let text = serde_json::to_string(&event).unwrap();
            let back: AgentEvent = serde_json::from_str(&text).unwrap();
            assert_eq!(
                serde_json::to_string(&back).unwrap(),
                text,
                "event did not survive a round trip"
            );
        }
    }

    #[test]
    fn step_limit_is_bounded_for_battery_safety() {
        assert!(MAX_STEPS <= 32, "an unattended loop must stay bounded");
        assert!(MAX_RETRIES <= 5);
    }
}
