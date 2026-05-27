use super::DesktopApp;
use super::session_launch;
use super::single_session::{SingleSessionApp, SingleSessionMessage};
use super::workspace;
use anyhow::{Context, Result};
use std::process::Command;
use std::time::Duration;

const TEMPORARY_DESKTOP_GALLERY_STATES: &[&str] = &[
    "empty",
    "markdown",
    "tool-running",
    "tool-success",
    "tool-failed",
    "tool-stack",
    "stdin-request",
    "streaming",
    "error",
    "model-picker",
    "long-transcript",
];

pub(super) fn launch_temporary_windows() -> Result<()> {
    let binary = std::env::current_exe().context("failed to resolve current desktop binary")?;
    for state in TEMPORARY_DESKTOP_GALLERY_STATES {
        Command::new(&binary)
            .arg("--gallery-state")
            .arg(state)
            .spawn()
            .with_context(|| format!("failed to launch gallery state {state}"))?;
        std::thread::sleep(Duration::from_millis(120));
    }
    println!(
        "launched {} temporary desktop gallery windows",
        TEMPORARY_DESKTOP_GALLERY_STATES.len()
    );
    Ok(())
}

fn temporary_gallery_title(state: &str) -> String {
    format!("Gallery · {state}")
}

pub(super) fn temporary_app(state: &str) -> DesktopApp {
    let mut app = SingleSessionApp::new(None);
    app.replace_session(Some(workspace::SessionCard {
        session_id: format!("gallery-{state}"),
        title: temporary_gallery_title(state),
        subtitle: "temporary desktop gallery".to_string(),
        detail: "fixture".to_string(),
        preview_lines: Vec::new(),
        detail_lines: Vec::new(),
        transcript_messages: Vec::new(),
    }));
    app.messages.push(SingleSessionMessage::meta(format!(
        "TEMP GALLERY STATE: {state}"
    )));
    match state {
        "empty" => app.set_status_label("empty fixture"),
        "markdown" => {
            app.messages.push(SingleSessionMessage::user(
                "Render markdown, code, and tables.",
            ));
            app.messages.push(SingleSessionMessage::assistant("# Markdown fixture\n\n- bullet one\n- **bold** and `inline code`\n\n| column | value |\n| --- | --- |\n| status | done |\n\n```rust\nfn gallery_fixture() { println!(\"hi\"); }\n```"));
            app.set_status_label("markdown fixture");
        }
        "tool-running" => {
            app.messages
                .push(SingleSessionMessage::user("Show a currently running tool."));
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolStarted {
                id: Some("gallery-running".to_string()),
                name: "bash".to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolExecuting {
                id: Some("gallery-running".to_string()),
                name: "bash".to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolInput {
                id: Some("gallery-running".to_string()),
                delta: r#"{"command":"cargo check -p jcode-desktop","run_in_background":true}"#
                    .to_string(),
            });
        }
        "tool-success" => {
            app.messages
                .push(SingleSessionMessage::user("Show a successful tool."));
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolStarted {
                id: Some("gallery-success".to_string()),
                name: "agentgrep".to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolExecuting {
                id: Some("gallery-success".to_string()),
                name: "agentgrep".to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolInput {
                id: Some("gallery-success".to_string()),
                delta: r#"{"query":"DesktopSessionEvent","path":"crates/jcode-desktop/src"}"#
                    .to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolFinished {
                id: Some("gallery-success".to_string()),
                name: "agentgrep".to_string(),
                summary: "matched 42 regions".to_string(),
                is_error: false,
            });
        }
        "tool-failed" => {
            app.messages
                .push(SingleSessionMessage::user("Show a failed tool."));
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolStarted {
                id: Some("gallery-failed".to_string()),
                name: "bash".to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolExecuting {
                id: Some("gallery-failed".to_string()),
                name: "bash".to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolFinished {
                id: Some("gallery-failed".to_string()),
                name: "bash".to_string(),
                summary: "exit code 101: compile error".to_string(),
                is_error: true,
            });
        }
        "tool-stack" => {
            app.messages.push(SingleSessionMessage::user(
                "Show a compact stack of completed tool calls.",
            ));
            app.messages
                .push(SingleSessionMessage::tool("▸ read done: loaded 100 lines"));
            app.messages
                .push(SingleSessionMessage::tool("▸ agentgrep done: 12 matches"));
            app.messages
                .push(SingleSessionMessage::tool("▸ edit done: updated renderer"));
            app.set_status_label("grouped tool stack fixture");
        }
        "stdin-request" => {
            app.messages.push(SingleSessionMessage::user(
                "Show interactive password input.",
            ));
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolStarted {
                id: Some("bash-call".to_string()),
                name: "bash".to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::ToolExecuting {
                id: Some("bash-call".to_string()),
                name: "bash".to_string(),
            });
            app.apply_session_event(session_launch::DesktopSessionEvent::StdinRequest {
                request_id: "fixture-stdin".to_string(),
                prompt: "Enter sudo password".to_string(),
                is_password: true,
                tool_call_id: "bash-call".to_string(),
            });
        }
        "streaming" => {
            app.messages
                .push(SingleSessionMessage::user("Show streaming text."));
            app.apply_session_event(session_launch::DesktopSessionEvent::TextReplace("This assistant response is still streaming. It should show receiving status and unfinished-response styling...".to_string()));
        }
        "error" => {
            app.messages
                .push(SingleSessionMessage::user("Show error state."));
            app.apply_session_event(session_launch::DesktopSessionEvent::Error(
                "Fixture backend error: socket disconnected".to_string(),
            ));
        }
        "model-picker" => {
            app.messages
                .push(SingleSessionMessage::user("Show model catalog/status."));
            app.apply_session_event(session_launch::DesktopSessionEvent::ModelCatalog {
                current_model: Some("gpt-5.1".to_string()),
                provider_name: Some("openai".to_string()),
                models: temporary_gallery_models(),
                reasoning_effort: Some("high".to_string()),
                service_tier: Some("priority".to_string()),
                compaction_mode: Some("auto".to_string()),
            });
            app.draft = "/model".to_string();
        }
        "long-transcript" => {
            for turn in 1..=8 {
                app.messages.push(SingleSessionMessage::user(format!(
                    "Prompt {turn}: long transcript fixture."
                )));
                app.messages.push(SingleSessionMessage::assistant("A longer assistant paragraph that wraps across multiple lines so scroll behavior, spacing, and transcript density are easy to inspect. Repeated content creates enough body height to verify scrollbar and bottom anchoring."));
            }
            app.set_status_label("long transcript fixture");
        }
        _ => app.set_status_label(format!("unknown gallery state {state}")),
    }
    app.draft_cursor = app.draft.len();
    app.scroll_body_to_bottom();
    DesktopApp::SingleSession(app)
}

fn temporary_gallery_models() -> Vec<session_launch::DesktopModelChoice> {
    vec![
        session_launch::DesktopModelChoice {
            model: "gpt-5.1".to_string(),
            provider: Some("openai".to_string()),
            api_method: Some("responses".to_string()),
            detail: Some("fixture current".to_string()),
            available: true,
        },
        session_launch::DesktopModelChoice {
            model: "claude-sonnet-4.5".to_string(),
            provider: Some("anthropic".to_string()),
            api_method: Some("messages".to_string()),
            detail: Some("fixture alternative".to_string()),
            available: true,
        },
    ]
}

pub(super) fn launcher_requested(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "--gallery" || arg == "--fixture-gallery")
}

pub(super) fn state_from_args(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--gallery-state=")
            .map(str::to_string)
            .or_else(|| {
                (arg == "--gallery-state")
                    .then(|| args.get(index + 1).cloned())
                    .flatten()
            })
    })
}
