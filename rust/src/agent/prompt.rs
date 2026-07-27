//! System prompt construction.
//!
//! The prompt is assembled per run rather than stored, because it encodes
//! things that change: which files already exist, what the device can afford,
//! and whether the model has a native tool-calling channel.
//!
//! Keep the stable text first. On cloud providers this prefix is cached, and a
//! volatile string near the top would invalidate the cache on every turn of the
//! loop — which is most of the cost of a long agent run.

use crate::governor::{DeviceProfile, DeviceTier};
use crate::llm::Provider;

pub struct PromptContext<'a> {
    pub project_name: &'a str,
    /// The VON number the user sees. Included so the agent names the workspace
    /// the same way the UI does.
    pub project_ordinal: i64,
    pub project_kind: &'a str,
    pub provider: Provider,
    pub existing_files: &'a [String],
    pub device: Option<&'a DeviceProfile>,
    /// Rolling summary of conversation that was dropped by context shifting.
    pub memory_summary: &'a str,
}

pub fn system_prompt(ctx: &PromptContext<'_>) -> String {
    let mut out = String::with_capacity(4096);

    // ---- stable prefix (cacheable) ----
    out.push_str(
        "You are VON, an autonomous coding agent running on the user's Android phone.\n\
         \n\
         You work inside a VON — an isolated sandbox holding one piece of work and its \
         conversation. The user sees it numbered and named, like \"VON 3 · Sales Dashboard\", \
         and that is the word to use when you refer to it. Everything you create lives inside \
         this VON and the user can export it as a zip. You cannot see or touch anything outside \
         it — not their photos, not their other VONs, not the wider filesystem.\n\
         \n\
         # How to work\n\
         \n\
         Plan briefly, then act. Write the files, run them, read the output, and fix what breaks. \
         Don't describe code you haven't written; write it. Don't claim something works until \
         you have run it and seen the result.\n\
         \n\
         Use paths relative to the VON's root: `main.py`, `src/util.py`. Absolute paths and \
         `..` are rejected.\n\
         \n\
         When a tool fails, read the error — it usually tells you exactly what to change. Try a \
         different approach rather than the same call twice.\n\
         \n\
         Call `task_complete` when the request is genuinely finished, with a short summary of \
         what you built. Don't call it with work still outstanding, and don't keep working after \
         you've called it.\n\
         \n\
         # Constraints of this environment\n\
         \n\
         - Python runs in an embedded interpreter with no network access and a short timeout. \
         The standard library is available; third-party packages are not — no numpy, no pandas. \
         Write plain Python.\n\
         - There is no shell, no package manager, and no git.\n\
         - Long-running work must be split into steps that each finish in a few seconds. A loop \
         that runs for a minute will be killed.\n\
         - Keep files modest. Very large files are rejected, and the user is on a phone.\n\
         \n",
    );

    match ctx.project_kind {
        "web" => out.push_str(
            "# This VON\n\n\
             A web VON. Write `index.html` at the root, plus any CSS and JS it needs. \
             Use plain HTML/CSS/JS — there is no build step and no npm. Once the page is ready, \
             call `start_preview` so the user can see it in the Preview tab.\n\n",
        ),
        "data" => out.push_str(
            "# This VON\n\n\
             A data/simulation VON. Write scripts that compute results and save them to files \
             (JSON or CSV) so the work survives the session. Run each script and check its output \
             before moving on. Finish with a `report.md` summarising what the numbers show.\n\n",
        ),
        "docs" => out.push_str(
            "# This VON\n\n\
             A document VON. Write Markdown files. Structure them with real headings, and \
             prefer concrete detail over filler.\n\n",
        ),
        _ => out.push_str(
            "# This VON\n\n\
             A general VON. Choose whatever file layout fits the request.\n\n",
        ),
    }

    // Local models have no native tool channel — spell out the wire format.
    if ctx.provider == Provider::Local {
        out.push_str(
            "# Calling tools\n\n\
             Emit a tool call as its own block, exactly:\n\
             <tool_call>{\"name\": \"write_file\", \"arguments\": {\"path\": \"main.py\", \
             \"content\": \"...\"}}</tool_call>\n\
             Then stop and wait — the results come back in the next message. Never invent tool \
             output yourself.\n\n",
        );
    }

    out.push_str(
        "# Style\n\n\
         Explain what you're doing in a sentence or two before each step, in plain language — \
         the user is watching on a phone screen. Lead with the outcome when you finish. Skip \
         preamble, skip restating the request, and don't narrate routine actions.\n\n\
         Deliver what was asked at the scope intended. Make routine judgement calls yourself; \
         don't add features, abstractions, or error handling nobody asked for. Finish the whole \
         task, and if part of it isn't possible here, say plainly what you left out and why.\n",
    );

    // ---- volatile suffix (never cached; must come last) ----
    // Named exactly as the UI shows it, so the agent and the user refer to the
    // same thing by the same handle.
    out.push_str(&format!(
        "\n# Current state\n\nWorking in: VON {} · {}\n",
        ctx.project_ordinal, ctx.project_name
    ));

    if let Some(device) = ctx.device {
        out.push_str(&format!(
            "Device: {} · {} cores · {:.1}GB RAM\n",
            device.tier.label(),
            device.cores,
            device.total_ram_mb as f32 / 1024.0
        ));
        if device.tier == DeviceTier::Low {
            out.push_str(
                "This is a low-end device. Keep programs small and fast; heavy computation will \
                 be throttled or killed.\n",
            );
        }
    }

    if ctx.existing_files.is_empty() {
        out.push_str("Files: this VON is empty.\n");
    } else {
        out.push_str(&format!(
            "Files already in this VON ({}):\n",
            ctx.existing_files.len()
        ));
        // Cap the listing: a VON with hundreds of files would otherwise crowd
        // out the conversation itself.
        for path in ctx.existing_files.iter().take(60) {
            out.push_str(&format!("  {path}\n"));
        }
        if ctx.existing_files.len() > 60 {
            out.push_str(&format!(
                "  … and {} more — call list_files for the full list.\n",
                ctx.existing_files.len() - 60
            ));
        }
    }

    if !ctx.memory_summary.trim().is_empty() {
        out.push_str(&format!(
            "\nEarlier in this VON:\n{}\n",
            ctx.memory_summary.trim()
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(kind: &'a str, provider: Provider, files: &'a [String]) -> PromptContext<'a> {
        PromptContext {
            project_name: "Sales Dashboard",
            project_ordinal: 3,
            project_kind: kind,
            provider,
            existing_files: files,
            device: None,
            memory_summary: "",
        }
    }

    #[test]
    fn web_projects_are_told_about_the_preview() {
        let prompt = system_prompt(&ctx("web", Provider::Anthropic, &[]));
        assert!(prompt.contains("index.html"));
        assert!(prompt.contains("start_preview"));
    }

    #[test]
    fn the_agent_is_given_the_same_von_handle_the_user_sees() {
        let prompt = system_prompt(&ctx("general", Provider::Anthropic, &[]));
        assert!(
            prompt.contains("VON 3 · Sales Dashboard"),
            "the agent should name the workspace exactly as the UI does"
        );
        // And it should know the word applies to the sandbox generally.
        assert!(prompt.contains("You work inside a VON"));
        assert!(prompt.contains("not their other VONs"));
    }

    #[test]
    fn only_local_models_get_the_tool_call_syntax() {
        assert!(system_prompt(&ctx("general", Provider::Local, &[])).contains("<tool_call>"));
        assert!(!system_prompt(&ctx("general", Provider::Anthropic, &[])).contains("<tool_call>"));
    }

    #[test]
    fn volatile_state_is_last_so_the_cached_prefix_survives() {
        let files = vec!["main.py".to_string()];
        let prompt = system_prompt(&ctx("general", Provider::Anthropic, &files));
        let state_at = prompt.find("# Current state").unwrap();
        let style_at = prompt.find("# Style").unwrap();
        assert!(style_at < state_at, "stable sections must precede volatile ones");
        assert!(prompt[state_at..].contains("main.py"));
    }

    #[test]
    fn long_file_lists_are_capped() {
        let files: Vec<String> = (0..200).map(|i| format!("f{i}.py")).collect();
        let prompt = system_prompt(&ctx("general", Provider::Anthropic, &files));
        assert!(prompt.contains("and 140 more"));
        assert!(!prompt.contains("f199.py"));
    }

    #[test]
    fn an_empty_sandbox_says_so_rather_than_listing_nothing() {
        assert!(system_prompt(&ctx("general", Provider::Anthropic, &[])).contains("sandbox is empty"));
    }

    #[test]
    fn low_end_devices_get_an_explicit_warning() {
        let device = DeviceProfile {
            tier: DeviceTier::Low,
            cores: 4,
            total_ram_mb: 3_000,
            available_ram_mb: 900,
            score: 30.0,
            max_local_params_b: 2.0,
            notes: vec![],
        };
        let mut c = ctx("general", Provider::Local, &[]);
        c.device = Some(&device);
        assert!(system_prompt(&c).contains("low-end device"));
    }
}
