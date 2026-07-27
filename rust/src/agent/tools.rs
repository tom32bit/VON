//! The agent's tool surface.
//!
//! Every tool is sandbox-scoped and returns a plain-text observation. Errors
//! come back as observations too (`is_error: true`) rather than aborting the
//! run — an agent that reads "PermissionDenied: use a relative path" and
//! retries is the entire point of the loop.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Result, VonError};
use crate::executor;
use crate::llm::{ToolCall, ToolResult, ToolSpec};
use crate::sandbox::Sandbox;
use crate::server;

/// Truncation cap for any single observation, so one `read_file` on a large
/// generated file can't consume the whole context window.
const MAX_OBSERVATION_BYTES: usize = 24 * 1024;

pub fn specs(project_kind: &str) -> Vec<ToolSpec> {
    let mut tools = vec![
        ToolSpec {
            name: "write_file".into(),
            description:
                "Create or overwrite a file in the project sandbox. Paths are relative to the \
                 project root (e.g. `src/main.py`). Parent directories are created for you. \
                 Overwrites without warning — read the file first if you need its current contents."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Sandbox-relative path, e.g. `src/main.py`." },
                    "content": { "type": "string", "description": "Full file contents." }
                },
                "required": ["path", "content"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: "read_file".into(),
            description:
                "Read a file from the project sandbox as text. Source and Markdown come back \
                 as-is; PDF and Word (.docx) documents the user imported are converted to text \
                 automatically, so read them exactly the same way."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Sandbox-relative path." }
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: "list_files".into(),
            description:
                "List every file in the project sandbox with its size. Call this first when \
                 resuming work, to see what already exists."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: "delete_file".into(),
            description: "Delete a file or directory from the project sandbox.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Sandbox-relative path." }
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: "run_python".into(),
            description:
                "Run Python in the embedded interpreter and return stdout, stderr and the exit \
                 code. The project sandbox is on `sys.path`, so you can import modules you wrote. \
                 No network access. Killed after the timeout. Use this to test code and to \
                 produce simulation data."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Python source to run. Mutually exclusive with `path`." },
                    "path": { "type": "string", "description": "Sandbox-relative .py file to run instead of inline code." },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Wall-clock budget, 100-30000ms. Defaults to 10000.",
                    }
                },
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: "run_wasm".into(),
            description:
                "Run a compiled WebAssembly module (WASI) from the sandbox. The sandbox is \
                 preopened at `/sandbox`, so the module can read and write project files."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Sandbox-relative path to a .wasm file." },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Command-line arguments passed to the module.",
                    },
                    "timeout_ms": { "type": "integer", "description": "Wall-clock budget, 100-30000ms." }
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        ToolSpec {
            name: "task_complete".into(),
            description:
                "Call this when the user's request is fully done. Summarise what you built and \
                 where it lives. Do not call it while work remains."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string", "description": "What you produced, in 1-3 sentences." }
                },
                "required": ["summary"],
                "additionalProperties": false,
            }),
        },
    ];

    // Only offered once an account is connected. An unusable tool in the list
    // is a tool a small model will eventually call and then apologise for.
    if crate::github::is_connected() {
        tools.push(ToolSpec {
            name: "git_push".into(),
            description:
                "Publish this project to GitHub as a single commit. Use it when the user asks \
                 to push, publish, or put the work on GitHub. The repository is created under \
                 their account if it does not exist, and secrets and dependency directories \
                 are filtered out automatically.\n\n\
                 This is a merge, not an overwrite: files added to the repository since the \
                 last sync are brought into this project rather than deleted. If the same file \
                 changed in both places nothing is pushed, and you are told which files \
                 diverged — report that to the user and let them decide."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "repo": {
                        "type": "string",
                        "description": "Repository as `name` (created if needed) or `owner/name` \
                                        (must already exist). Ask the user if they have not said.",
                    },
                    "message": { "type": "string", "description": "Commit message describing this change." },
                    "branch": { "type": "string", "description": "Branch name. Defaults to `main`." },
                    "private": { "type": "boolean", "description": "Create the repository private. Defaults to true." }
                },
                "required": ["repo"],
                "additionalProperties": false,
            }),
        });
        tools.push(ToolSpec {
            name: "git_pull".into(),
            description:
                "Bring a GitHub repository's files into this project without pushing anything \
                 back. Use it to start from existing code, or to catch up before continuing \
                 work. Refuses rather than overwriting if a file differs in both places."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "repo": {
                        "type": "string",
                        "description": "Repository as `owner/name`, or `name` for one of the \
                                        user's own.",
                    },
                    "branch": { "type": "string", "description": "Branch name. Defaults to `main`." }
                },
                "required": ["repo"],
                "additionalProperties": false,
            }),
        });
    }

    // Only offer the preview server where it makes sense; an unused tool in the
    // list is a tool a small model will eventually call by mistake.
    if project_kind == "web" {
        tools.push(ToolSpec {
            name: "start_preview".into(),
            description:
                "Serve the sandbox over http://127.0.0.1 and return the URL, so the user can see \
                 the site in the Preview tab. Requires an `index.html`. Call this once the page \
                 is ready."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        });
    }
    tools
}

/// What a tool call did, beyond its text output — the loop uses this to drive
/// UI side effects and to decide when to stop.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolEffects {
    /// The file tree changed and the Sandbox Explorer should refresh.
    pub files_changed: bool,
    /// A preview server came up at this URL.
    pub preview_url: Option<String>,
    /// The agent declared the task finished.
    pub completed: Option<String>,
}

pub struct Execution {
    pub result: ToolResult,
    pub effects: ToolEffects,
}

/// Runs one tool call. Never returns `Err` for a tool-level failure — those are
/// folded into the observation so the agent can react to them.
pub fn execute(sandbox: &Sandbox, project_id: &str, call: &ToolCall) -> Execution {
    let mut effects = ToolEffects::default();

    let outcome = match call.name.as_str() {
        "write_file" => write_file(sandbox, &call.arguments).inspect(|_| {
            effects.files_changed = true;
        }),
        "read_file" => read_file(sandbox, &call.arguments),
        "list_files" => list_files(sandbox),
        "delete_file" => delete_file(sandbox, &call.arguments).inspect(|_| {
            effects.files_changed = true;
        }),
        "run_python" => run_python(sandbox, &call.arguments).inspect(|_| {
            // Scripts routinely write output files.
            effects.files_changed = true;
        }),
        "run_wasm" => run_wasm(sandbox, &call.arguments).inspect(|_| {
            effects.files_changed = true;
        }),
        "start_preview" => start_preview(sandbox, project_id).map(|url| {
            effects.preview_url = Some(url.clone());
            format!("Preview is live at {url}. The user can open the Preview tab to see it.")
        }),
        "git_push" => git_push(sandbox, project_id, &call.arguments),
        // Files land in the sandbox, so the explorer has to refresh.
        "git_pull" => git_pull(sandbox, project_id, &call.arguments).inspect(|_| {
            effects.files_changed = true;
        }),
        "task_complete" => {
            let summary = string_arg(&call.arguments, "summary").unwrap_or_default();
            effects.completed = Some(summary.clone());
            Ok(format!("Acknowledged: {summary}"))
        }
        unknown => Err(VonError::InvalidArgument(format!(
            "there is no tool called '{unknown}'. Available tools: write_file, read_file, \
             list_files, delete_file, run_python, run_wasm, task_complete."
        ))),
    };

    let (content, is_error) = match outcome {
        Ok(text) => (truncate_observation(text), false),
        // agent_hint is written to tell the model what to do differently.
        Err(e) => (e.agent_hint(), true),
    };

    Execution {
        result: ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content,
            is_error,
        },
        effects,
    }
}

/// A one-line description for the "Calling Tool" row in the UI.
pub fn describe(call: &ToolCall) -> String {
    let arg = |key: &str| string_arg(&call.arguments, key).unwrap_or_default();
    match call.name.as_str() {
        "write_file" => format!("Writing {}", arg("path")),
        "read_file" => format!("Reading {}", arg("path")),
        "list_files" => "Listing project files".into(),
        "delete_file" => format!("Deleting {}", arg("path")),
        "run_python" => {
            let path = arg("path");
            if path.is_empty() {
                "Running Python".into()
            } else {
                format!("Running {path}")
            }
        }
        "run_wasm" => format!("Running {}", arg("path")),
        "start_preview" => "Starting live preview".into(),
        "git_push" | "git_pull" => {
            let verb = if call.name == "git_push" {
                "Syncing with"
            } else {
                "Pulling from"
            };
            let repo = arg("repo");
            if repo.is_empty() {
                format!("{verb} GitHub")
            } else {
                format!("{verb} {repo}")
            }
        }
        "task_complete" => "Finishing up".into(),
        other => format!("Calling {other}"),
    }
}

// ---- individual tools ---------------------------------------------------

fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn require_string(args: &Value, key: &str) -> Result<String> {
    string_arg(args, key).ok_or_else(|| {
        VonError::InvalidArgument(format!("the '{key}' argument is required and must be a string"))
    })
}

fn optional_timeout(args: &Value) -> Option<u64> {
    args.get("timeout_ms").and_then(|v| v.as_u64())
}

fn write_file(sandbox: &Sandbox, args: &Value) -> Result<String> {
    let path = require_string(args, "path")?;
    // Accept a missing `content` as an empty file rather than erroring: models
    // legitimately create placeholder files this way.
    let content = string_arg(args, "content").unwrap_or_default();
    let bytes = sandbox.write_file(&path, &content)?;
    Ok(format!(
        "Wrote {bytes} bytes to {path} ({} lines).",
        content.lines().count()
    ))
}

/// Reads a file as text, converting documents on the way through.
///
/// The agent has exactly one way to look at a file and it returns UTF-8, so an
/// imported PDF or Word document would otherwise be unreadable — which would
/// defeat the point of importing one. Conversion happens here rather than at
/// import so the original stays on disk: still exportable, still pushable to
/// GitHub, still the file the user recognises in the Files tab.
fn read_file(sandbox: &Sandbox, args: &Value) -> Result<String> {
    let path = require_string(args, "path")?;
    if !crate::documents::needs_extraction(&path) {
        let content = sandbox.read_file(&path)?;
        return Ok(format!("--- {path} ---\n{content}"));
    }
    let bytes = sandbox.read_bytes(&path)?;
    let text = crate::documents::extract(&path, &bytes)?;
    Ok(format!(
        "--- {path} (text extracted from the original document) ---\n{text}"
    ))
}

fn list_files(sandbox: &Sandbox) -> Result<String> {
    let files = sandbox.list_flat()?;
    if files.is_empty() {
        return Ok("The project sandbox is empty — no files created yet.".into());
    }
    let mut out = format!("{} file(s) in the project:\n", files.len());
    for path in &files {
        let size = sandbox
            .resolve(path)
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        out.push_str(&format!("  {path} ({size} bytes)\n"));
    }
    Ok(out)
}

fn delete_file(sandbox: &Sandbox, args: &Value) -> Result<String> {
    let path = require_string(args, "path")?;
    sandbox.delete(&path)?;
    Ok(format!("Deleted {path}."))
}

fn run_python(sandbox: &Sandbox, args: &Value) -> Result<String> {
    let timeout = optional_timeout(args);
    let outcome = match (string_arg(args, "code"), string_arg(args, "path")) {
        (Some(code), _) if !code.trim().is_empty() => {
            executor::run_python(sandbox, &code, timeout)?
        }
        (_, Some(path)) if !path.trim().is_empty() => {
            executor::run_python_file(sandbox, &path, timeout)?
        }
        _ => {
            return Err(VonError::InvalidArgument(
                "pass either 'code' (inline source) or 'path' (a .py file in the sandbox)".into(),
            ))
        }
    };
    Ok(outcome.as_observation())
}

fn run_wasm(sandbox: &Sandbox, args: &Value) -> Result<String> {
    let path = require_string(args, "path")?;
    let wasm_args: Vec<String> = args
        .get("args")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let outcome = executor::run_wasm(sandbox, &path, &wasm_args, optional_timeout(args))?;
    Ok(outcome.as_observation())
}

fn start_preview(sandbox: &Sandbox, project_id: &str) -> Result<String> {
    if !sandbox.exists("index.html") {
        return Err(VonError::NotFound(
            "there is no index.html in the project root — the preview server needs one to \
             serve. Write it first."
                .into(),
        ));
    }
    server::start(project_id, sandbox.clone())
}

/// Syncs the sandbox with GitHub.
///
/// `private` defaults to true. A model that omits the flag is not making a
/// judgement about who should see the code, and the safe reading of silence is
/// "not the whole internet" — the user can flip it on GitHub in two clicks,
/// which is the easier direction to travel.
fn git_push(sandbox: &Sandbox, project_id: &str, args: &Value) -> Result<String> {
    let repo = require_repo(args)?;
    let outcome = crate::github::sync_sandbox(
        project_id,
        sandbox,
        &repo,
        &string_arg(args, "branch").unwrap_or_default(),
        &string_arg(args, "message").unwrap_or_default(),
        args.get("private").and_then(Value::as_bool).unwrap_or(true),
        |_| {},
    )?;

    if outcome.up_to_date {
        return Ok(format!(
            "{} was already up to date; there was nothing to commit.",
            outcome.repo_full_name
        ));
    }

    let mut out = format!(
        "Synced with {} on branch {}. Commit {}.\n{}",
        outcome.repo_full_name,
        outcome.branch,
        &outcome.commit_sha[..outcome.commit_sha.len().min(7)],
        outcome.commit_url,
    );
    if outcome.created_repo {
        out.push_str("\nThe repository did not exist and was created.");
    }
    append_sync_detail(&mut out, &outcome.pulled, &outcome.removed_locally);
    if !outcome.skipped.is_empty() {
        // Named rather than summarised: the agent should be able to tell the
        // user which file is missing from the repo and why.
        out.push_str("\n\nLeft out on purpose:\n");
        for reason in &outcome.skipped {
            out.push_str(&format!("  - {reason}\n"));
        }
    }
    Ok(out)
}

/// Brings a repository's files into the sandbox without pushing anything back.
fn git_pull(sandbox: &Sandbox, project_id: &str, args: &Value) -> Result<String> {
    let repo = require_repo(args)?;
    let outcome = crate::github::pull_into_sandbox(
        project_id,
        sandbox,
        &repo,
        &string_arg(args, "branch").unwrap_or_default(),
        |_| {},
    )?;

    if outcome.pulled.is_empty() && outcome.removed_locally.is_empty() {
        return Ok(format!(
            "This project already matches {} on branch {}.",
            outcome.repo_full_name, outcome.branch
        ));
    }
    let mut out = format!(
        "Pulled {} at {}.",
        outcome.repo_full_name,
        &outcome.commit_sha[..outcome.commit_sha.len().min(7)],
    );
    append_sync_detail(&mut out, &outcome.pulled, &outcome.removed_locally);
    out.push_str("\n\nCall list_files to see the project as it now stands.");
    Ok(out)
}

fn require_repo(args: &Value) -> Result<String> {
    let repo = string_arg(args, "repo").unwrap_or_default();
    if repo.trim().is_empty() {
        return Err(VonError::InvalidArgument(
            "which repository? Pass `repo` as `name` to use one under the user's account, or \
             `owner/name` for somebody else's. Ask the user if they have not said."
                .into(),
        ));
    }
    Ok(repo)
}

/// Spells out what the merge did to the sandbox.
///
/// The agent has to be able to answer "what changed?" without calling
/// `list_files` again, and a file that appeared or vanished underneath it is
/// exactly what would otherwise confuse it on the next step.
fn append_sync_detail(out: &mut String, pulled: &[String], removed: &[String]) {
    if !pulled.is_empty() {
        out.push_str("\n\nBrought in from GitHub:\n");
        for path in pulled {
            out.push_str(&format!("  - {path}\n"));
        }
    }
    if !removed.is_empty() {
        out.push_str("\nDeleted on GitHub, so removed here too:\n");
        for path in removed {
            out.push_str(&format!("  - {path}\n"));
        }
    }
}

fn truncate_observation(mut text: String) -> String {
    if text.len() <= MAX_OBSERVATION_BYTES {
        return text;
    }
    let mut cut = MAX_OBSERVATION_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str("\n\n… [output truncated. Read the file in smaller pieces if you need more.]");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Sandbox) {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::open(dir.path().join("proj")).unwrap();
        (dir, sb)
    }

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: name.into(),
            arguments: args,
            thought_signature: String::new(),
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let (_d, sb) = setup();
        let write = execute(&sb, "p", &call("write_file", json!({"path":"a.py","content":"x=1"})));
        assert!(!write.result.is_error);
        assert!(write.effects.files_changed);

        let read = execute(&sb, "p", &call("read_file", json!({"path":"a.py"})));
        assert!(read.result.content.contains("x=1"));
    }

    #[test]
    fn escaping_the_sandbox_returns_an_instructional_observation() {
        let (_d, sb) = setup();
        let out = execute(&sb, "p", &call("write_file", json!({"path":"../evil","content":"x"})));
        assert!(out.result.is_error);
        // The agent must learn the rule, not just that it failed.
        assert!(out.result.content.contains("relative to the project root"));
    }

    #[test]
    fn an_unknown_tool_lists_the_real_ones() {
        let (_d, sb) = setup();
        let out = execute(&sb, "p", &call("rm_rf", json!({})));
        assert!(out.result.is_error);
        assert!(out.result.content.contains("write_file"));
    }

    #[test]
    fn missing_required_arguments_are_reported_clearly() {
        let (_d, sb) = setup();
        let out = execute(&sb, "p", &call("read_file", json!({})));
        assert!(out.result.is_error);
        assert!(out.result.content.contains("'path' argument is required"));
    }

    #[test]
    fn task_complete_reports_the_summary_as_an_effect() {
        let (_d, sb) = setup();
        let out = execute(&sb, "p", &call("task_complete", json!({"summary":"Built it."})));
        assert_eq!(out.effects.completed.as_deref(), Some("Built it."));
        assert!(!out.result.is_error);
    }

    #[test]
    fn preview_requires_an_index_html() {
        let (_d, sb) = setup();
        let out = execute(&sb, "p", &call("start_preview", json!({})));
        assert!(out.result.is_error);
        assert!(out.result.content.contains("index.html"));
    }

    #[test]
    fn listing_an_empty_project_says_so() {
        let (_d, sb) = setup();
        let out = execute(&sb, "p", &call("list_files", json!({})));
        assert!(out.result.content.contains("empty"));
    }

    #[test]
    fn observations_are_capped() {
        let huge = "z".repeat(MAX_OBSERVATION_BYTES * 2);
        let out = truncate_observation(huge);
        assert!(out.len() < MAX_OBSERVATION_BYTES + 200);
        assert!(out.contains("truncated"));
    }

    #[test]
    fn preview_tool_is_only_offered_to_web_projects() {
        assert!(specs("web").iter().any(|t| t.name == "start_preview"));
        assert!(!specs("general").iter().any(|t| t.name == "start_preview"));
    }

    #[test]
    fn every_spec_has_an_object_schema() {
        for kind in ["web", "general", "data"] {
            for spec in specs(kind) {
                assert_eq!(spec.parameters["type"], json!("object"), "{}", spec.name);
                assert!(!spec.description.is_empty(), "{}", spec.name);
            }
        }
    }
}
