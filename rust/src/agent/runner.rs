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
use crate::error::{ErrorInfo, Result};
use crate::governor;
use crate::llm::{
    self,
    ChatMessage,
    Delta,
    GenerateRequest,
    Provider,
    Role,
    StopReason,
    ToolCall,
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

/// How many times a turn may be asked to actually do the thing it just said it
/// would do.
///
/// Open models routed through an OpenAI-compatible endpoint narrate a lot:
/// "I will read the Design.md file to get the design details" and then stop,
/// having called nothing. Treating that as the end of the turn is technically
/// what the model asked for and completely useless to the user, who is left
/// looking at a promise. Two nudges, then take the answer as final — a model
/// that will not act after being told twice is not going to on the third.
const MAX_NUDGES: u32 = 2;

/// Sent back as a user turn when the model narrates instead of acting.
///
/// Phrased so a model that genuinely *has* finished can close cleanly rather
/// than being badgered into inventing work: `task_complete` is one cheap call
/// and it is what the system prompt already asks for.
const NUDGE: &str = "You ended your turn without calling a tool. If you described something you \
     are about to do, do it now — call the tool. If the request is genuinely finished, call \
     task_complete with a summary. Do not reply with prose alone.";

// An unattended loop on someone's phone must stay bounded. Checked at compile
// time rather than in a test, so raising either past the ceiling fails the
// build instead of a test run.
const _: () = assert!(MAX_STEPS <= 32);
const _: () = assert!(MAX_RETRIES <= 5);

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
    let engine = llm::engine_for(provider, &project.model_id)?;
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
    // Held for the whole run so backgrounding does not shed the model out
    // from under it. Dropped on every exit path, including the early ones.
    let _run = state::hold_active_run();

    let mut steps: u32 = 0;
    let mut retries = 0usize;
    let mut nudges = 0u32;
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
            // A model that stopped mid-thought rather than finishing gets told
            // so. Not persisted: the nudge is scaffolding for the model, not
            // part of the conversation the user should have to read back.
            let stalled = completion.stop_reason != StopReason::Cancelled
                && completion.stop_reason != StopReason::MaxTokens
                && !cancelled()
                && !tool_specs.is_empty();
            if stalled && nudges < MAX_NUDGES {
                nudges += 1;
                emit(AgentEvent::Phase {
                    phase: "retrying".into(),
                    detail: "Asking the model to take the action it described".into(),
                });
                messages.push(ChatMessage::user(NUDGE));
                continue;
            }

            final_summary = completion.text;
            stop_label = match completion.stop_reason {
                StopReason::MaxTokens => "max_tokens",
                StopReason::Cancelled => "cancelled",
                _ => "end_turn",
            };
            break;
        }
        // It acted, so the allowance resets — a later stall is a fresh problem.
        nudges = 0;

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

/// Rebuilds the in-memory transcript from SQLite, repairing it on the way.
///
/// Every bug that has made a VON permanently unusable has had the same shape: a
/// malformed value reached the transcript, and the transcript is replayed in
/// full on every subsequent turn, so one bad row failed every future request
/// rather than just the one that produced it. The user's only escape was to
/// clear the conversation and lose the context.
///
/// Repair belongs here rather than only at the point each bug was found. The
/// send path can be made careful, but it cannot fix rows written before it was;
/// this runs against whatever is actually on disk, including rows written by
/// builds that no longer exist.
fn load_transcript(project_id: &str) -> Result<Vec<ChatMessage>> {
    let rows = db::list_messages(project_id)?;
    let mut out: Vec<ChatMessage> = Vec::with_capacity(rows.len());

    for row in rows {
        let role = Role::parse(&row.role);
        let message = match role {
            Role::Assistant => {
                let calls: Vec<ToolCall> =
                    serde_json::from_str(&row.steps_json).unwrap_or_default();
                ChatMessage::assistant(row.content, calls.into_iter().filter_map(repair_call).collect())
            }
            Role::Tool => {
                ChatMessage::tool(serde_json::from_str(&row.steps_json).unwrap_or_default())
            }
            _ => ChatMessage {
                role,
                text: row.content,
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
            },
        };
        out.push(message);
    }

    close_open_tool_calls(&mut out);
    Ok(out)
}

/// Drops a tool call no provider would accept, and normalises the rest.
///
/// A call with no name cannot be replayed at all — there is nothing to send.
/// Arguments that are not an object are the NVIDIA 400 in its stored form: the
/// value parses as JSON and is still rejected, on every turn, forever.
fn repair_call(mut call: ToolCall) -> Option<ToolCall> {
    if call.name.trim().is_empty() {
        return None;
    }
    if !call.arguments.is_object() {
        call.arguments = serde_json::json!({});
    }
    if call.id.trim().is_empty() {
        call.id = format!("repaired-{}", uuid::Uuid::new_v4());
    }
    Some(call)
}

/// Answers any tool call the transcript left hanging.
///
/// Every provider rejects an assistant turn carrying a tool call with no
/// matching result. The run loop is careful to record one even on cancellation
/// and error, but it cannot help when Android kills the process between the two
/// writes — and the row that survives is exactly the one that breaks the next
/// request. Synthesising the missing result costs a sentence of context and
/// turns a permanently broken VON into one that simply continues.
fn close_open_tool_calls(messages: &mut Vec<ChatMessage>) {
    let mut index = 0;
    while index < messages.len() {
        if messages[index].role != Role::Assistant || messages[index].tool_calls.is_empty() {
            index += 1;
            continue;
        }

        let answered: std::collections::HashSet<String> = messages
            .get(index + 1)
            .filter(|next| next.role == Role::Tool)
            .map(|next| {
                next.tool_results
                    .iter()
                    .map(|r| r.call_id.clone())
                    .collect()
            })
            .unwrap_or_default();

        let missing: Vec<ToolResult> = messages[index]
            .tool_calls
            .iter()
            .filter(|call| !answered.contains(&call.id))
            .map(|call| ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: "This tool did not finish — the app was closed while it was running. \
                          Run it again if the result still matters."
                    .to_string(),
                is_error: true,
            })
            .collect();

        if missing.is_empty() {
            index += 1;
            continue;
        }

        match messages.get_mut(index + 1) {
            // Extend the existing result turn rather than inserting a second
            // one: two tool turns in a row is its own protocol violation.
            Some(next) if next.role == Role::Tool => next.tool_results.extend(missing),
            _ => messages.insert(index + 1, ChatMessage::tool(missing)),
        }
        index += 1;
    }
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

    // ---- transcript repair ------------------------------------------------
    //
    // Every bug that made a VON permanently unusable had the same shape: a bad
    // row reached the transcript, and the transcript is replayed in full on
    // every later turn. These cover the two shapes seen in the wild.

    fn assistant_calling(calls: Vec<ToolCall>) -> ChatMessage {
        ChatMessage::assistant("working on it", calls)
    }

    fn a_call(id: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "list_files".into(),
            arguments,
            thought_signature: String::new(),
        }
    }

    #[test]
    fn a_stored_null_argument_list_is_repaired_to_an_object() {
        let call = repair_call(a_call("1", serde_json::Value::Null)).unwrap();
        assert_eq!(call.arguments, serde_json::json!({}));

        // And a real one is left alone.
        let kept = repair_call(a_call("1", serde_json::json!({"path": "a.py"}))).unwrap();
        assert_eq!(kept.arguments, serde_json::json!({"path": "a.py"}));
    }

    #[test]
    fn a_nameless_call_is_dropped_rather_than_replayed() {
        let mut call = a_call("1", serde_json::json!({}));
        call.name = "  ".into();
        assert!(repair_call(call).is_none(), "there is nothing to send");
    }

    #[test]
    fn a_tool_call_the_process_died_on_gets_an_answer() {
        // Killed between writing the assistant turn and writing its result.
        // Every provider rejects that pairing, so without repair the VON is
        // broken for good.
        let mut messages = vec![
            ChatMessage::user("go"),
            assistant_calling(vec![a_call("call-1", serde_json::json!({}))]),
        ];
        close_open_tool_calls(&mut messages);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].role, Role::Tool);
        assert_eq!(messages[2].tool_results[0].call_id, "call-1");
        assert!(messages[2].tool_results[0].is_error);
    }

    #[test]
    fn a_partly_answered_turn_is_topped_up_not_duplicated() {
        let mut messages = vec![
            assistant_calling(vec![
                a_call("call-1", serde_json::json!({})),
                a_call("call-2", serde_json::json!({})),
            ]),
            ChatMessage::tool(vec![ToolResult {
                call_id: "call-1".into(),
                name: "list_files".into(),
                content: "one file".into(),
                is_error: false,
            }]),
        ];
        close_open_tool_calls(&mut messages);

        // Two tool turns in a row would be its own protocol violation.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].tool_results.len(), 2);
        assert!(!messages[1].tool_results[0].is_error, "the real result stands");
        assert_eq!(messages[1].tool_results[1].call_id, "call-2");
    }

    #[test]
    fn a_complete_transcript_is_left_exactly_as_it_was() {
        let mut messages = vec![
            ChatMessage::user("go"),
            assistant_calling(vec![a_call("call-1", serde_json::json!({}))]),
            ChatMessage::tool(vec![ToolResult {
                call_id: "call-1".into(),
                name: "list_files".into(),
                content: "done".into(),
                is_error: false,
            }]),
        ];
        let before = messages.len();
        close_open_tool_calls(&mut messages);
        assert_eq!(messages.len(), before);
        assert_eq!(messages[2].tool_results.len(), 1);
    }

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
                thought_signature: String::new(),
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

}
