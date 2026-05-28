use super::Agent;
use crate::logging;
use crate::message::{Message, ToolDefinition};
use crate::prompt::ContextInfo;
use serde_json::json;
use std::path::Path;
use dirs;

/// Maximum characters for enrich task context to prevent massive Python bridge payloads
const MAX_ENRICH_TASK_CHARS: usize = 10_000;

impl Agent {
    /// Log the user prompt for debugging enrichment issues
    pub(super) fn log_user_prompt_for_enrich(&self, task: &str) {
        let preview: String = task.chars().take(200).collect();
        logging::info(&format!(
            "User prompt for enrich: {}{} ({} chars total)",
            preview,
            if task.len() > 200 { "..." } else { "" },
            task.len()
        ));
    }

    pub(super) fn log_prompt_prefix_accounting(
        &self,
        split: &crate::prompt::SplitSystemPrompt,
        tools: &[ToolDefinition],
        context_info: Option<&ContextInfo>,
    ) {
        let system_tokens = split.estimated_tokens();
        let tool_tokens = ToolDefinition::aggregate_prompt_token_estimate(tools);
        let prefix_tokens = system_tokens + tool_tokens;

        if let Some(info) = context_info {
            let breakdown: Vec<String> = info
                .breakdown()
                .iter()
                .map(|(label, chars, _icon)| format!("{}={}B", label, chars))
                .collect();
            logging::info(&format!(
                "Prompt prefix: total={} tok (sys={} tools={}) | [{:}]",
                prefix_tokens,
                system_tokens,
                tool_tokens,
                breakdown.join(", "),
            ));
        } else {
            logging::info(&format!(
                "Prompt prefix estimate: total={} tokens (system={} tools={})",
                prefix_tokens, system_tokens, tool_tokens
            ));
        }
    }

    pub(super) fn build_memory_prompt_nonblocking_shared(
        &self,
        messages: std::sync::Arc<[Message]>,
        _memory_event_tx: Option<crate::memory::MemoryEventSink>,
    ) -> Option<crate::memory::PendingMemory> {
        if !self.memory_enabled {
            return None;
        }

        let session_id = &self.session.id;

        let pending = if crate::message::ends_with_fresh_user_turn(&messages) {
            crate::memory::take_pending_memory(session_id)
        } else {
            None
        };

        // Use the persistent memory-agent pipeline as the single source of truth.
        // Running both this and the legacy MemoryManager background retrieval path
        // can prepare overlapping pending prompts for the same turn, which makes
        // memory injection feel overly aggressive.
        crate::memory_agent::update_context_sync_with_dir(
            session_id,
            messages,
            self.session.working_dir.clone(),
        );

        pending
    }

    fn append_current_turn_system_reminder(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        let Some(reminder) = self
            .current_turn_system_reminder
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        else {
            return;
        };

        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str("# System Reminder\n\n");
        split.dynamic_part.push_str(reminder);
    }

    /// Build split system prompt for better caching
    /// Returns static (cacheable) and dynamic (not cached) parts separately,
    /// along with ContextInfo describing what was loaded.
    pub(super) fn build_system_prompt_split(
        &self,
        memory_prompt: Option<&str>,
    ) -> (crate::prompt::SplitSystemPrompt, ContextInfo) {
        if let Some(ref override_prompt) = self.system_prompt_override {
            return (
                crate::prompt::SplitSystemPrompt {
                    static_part: override_prompt.clone(),
                    dynamic_part: String::new(),
                },
                ContextInfo::default(),
            );
        }

        let skills = self.current_skills_snapshot();
        let skill_prompt = self
            .active_skill
            .as_ref()
            .and_then(|name| skills.get(name).map(|skill| skill.get_prompt().to_string()));

        let available_skills: Vec<crate::prompt::SkillInfo> = self
            .current_skills_snapshot()
            .list()
            .iter()
            .map(|skill| crate::prompt::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect();

        let working_dir = self
            .session
            .working_dir
            .as_ref()
            .map(std::path::PathBuf::from);

        let (mut split, context_info) = crate::prompt::build_system_prompt_split(
            skill_prompt.as_deref(),
            &available_skills,
            self.session.is_canary,
            memory_prompt,
            working_dir.as_deref(),
        );

        self.append_current_turn_system_reminder(&mut split);

        (split, context_info)
    }

    /// Non-blocking memory prompt - takes pending result and spawns check for next turn
    pub(super) fn build_memory_prompt_nonblocking(
        &self,
        messages: &[Message],
        _memory_event_tx: Option<crate::memory::MemoryEventSink>,
    ) -> Option<crate::memory::PendingMemory> {
        self.build_memory_prompt_nonblocking_shared(messages.to_vec().into(), _memory_event_tx)
    }

    /// AUTO-CALL Mimir enrich_task at application layer (non-negotiable enforcement).
    /// This runs BEFORE the model sees the turn, ensuring Mimir context is always injected.
    /// Returns the enriched context string, or None if Mimir is not configured.
    pub(super) async fn auto_enrich_task(&self) -> Option<String> {
        use std::path::Path;

        crate::logging::info("Auto-enrich: starting auto_enrich_task()");

        // Extract conversation context: use the latest user message for enrich_task
        // so Mimir enriches based on the current request, not the original.
        let user_task = self.extract_latest_user_message().unwrap_or_default();

        // Log the user prompt for debugging
        self.log_user_prompt_for_enrich(&user_task);

        crate::logging::info(&format!(
            "Auto-enrich: extracted user task ({} chars): {}",
            user_task.len(),
            &user_task[..user_task.len().min(80)]
        ));

        if user_task.is_empty() {
            crate::logging::info("Auto-enrich: user task is empty, skipping");
            return None;
        }

        // Find the Mimir bridge (reuse detection logic from mimir.rs)
        let working_dir = self.session.working_dir.as_deref().map(Path::new);
        crate::logging::debug(&format!(
            "Auto-enrich: detecting bridge path for working_dir={:?}",
            working_dir
        ));
        let bridge_path = match detect_mimir_bridge(working_dir) {
            Some(path) => {
                crate::logging::debug(&format!(
                    "Auto-enrich: bridge path detected at {}",
                    path.display()
                ));
                path
            }
            None => {
                crate::logging::warn("Auto-enrich: no Mimir bridge detected, skipping");
                return None;
            }
        };

        // Find Python
        let python = find_python();
        crate::logging::debug(&format!("Auto-enrich: using Python binary '{}'", python));

        // Build the enrich_task request with full conversation context
        let request = json!({
            "action": "enrich_task",
            "params": {
                "task": user_task
            }
        });
        let request_str = serde_json::to_string(&request).ok()?;
        crate::logging::debug(&format!(
            "Auto-enrich: sending request ({} bytes): {}",
            request_str.len(),
            &request_str[..request_str.len().min(200)]
        ));

        crate::logging::info(&format!(
            "Auto-enrich: calling Mimir enrich_task for task: {}",
            &user_task[..user_task.len().min(100)]
        ));

        let started = std::time::Instant::now();

        let mut cmd = tokio::process::Command::new(python);
        cmd.arg(&bridge_path);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if let Some(ref cwd) = working_dir {
            cmd.env("PROJECT_ROOT", cwd.display().to_string());
            crate::logging::debug(&format!(
                "Auto-enrich: setting PROJECT_ROOT={}",
                cwd.display()
            ));
        }
        if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
            cmd.env("OPENROUTER_API_KEY", key);
        } else if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            cmd.env("OPENAI_API_KEY", key);
        } else {
            crate::logging::debug("Auto-enrich: no OPENROUTER_API_KEY or OPENAI_API_KEY found in env");
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                crate::logging::warn(&format!("Auto-enrich: failed to spawn Python bridge: {}", e));
                return None;
            }
        };

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = stdin.write_all(request_str.as_bytes()).await {
                crate::logging::warn(&format!(
                    "Auto-enrich: failed to write request to bridge stdin: {}",
                    e
                ));
                return None;
            }
            stdin.shutdown().await.ok();
        }

        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(120),
            child.wait_with_output(),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                crate::logging::warn(&format!(
                    "Auto-enrich: Mimir bridge error: {}",
                    e
                ));
                return None;
            }
            Err(_) => {
                crate::logging::warn("Auto-enrich: Mimir bridge timed out after 120s");
                return None;
            }
        };

        let elapsed_ms = started.elapsed().as_millis();
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        crate::logging::debug(&format!(
            "Auto-enrich: bridge exited with code {} after {}ms (stdout={}B stderr={}B)",
            exit_code,
            elapsed_ms,
            stdout.len(),
            stderr.len()
        ));

        if !stderr.is_empty() {
            let stderr_preview: String = stderr.chars().take(500).collect();
            let stderr_preview = if stderr.len() > 500 {
                format!("{}...", stderr_preview)
            } else {
                stderr_preview
            };
            crate::logging::debug(&format!("Auto-enrich: bridge stderr: {}", stderr_preview));
        }

        if !output.status.success() {
            crate::logging::warn(&format!(
                "Auto-enrich: Mimir bridge failed after {}ms (exit_code={}) (non-fatal)",
                elapsed_ms, exit_code
            ));
            return None;
        }

        let stdout_preview: String = stdout.chars().take(500).collect();
        let stdout_preview = if stdout.len() > 500 {
            format!("{}...", stdout_preview)
        } else {
            stdout_preview
        };
        crate::logging::debug(&format!(
            "Auto-enrich: raw bridge response ({} bytes): {}",
            stdout.len(),
            stdout_preview
        ));

        let stdout_str = stdout.as_ref();

        let response: serde_json::Value = match serde_json::from_str(stdout_str) {
            Ok(v) => v,
            Err(e) => {
                crate::logging::warn(&format!(
                    "Auto-enrich: failed to parse Mimir response: {}",
                    e
                ));
                crate::logging::debug(&format!(
                    "Auto-enrich: unparseable response text: {}",
                    stdout_preview
                ));
                return None;
            }
        };

        let status = response
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown");
        let routed_to = response
            .get("routed_to")
            .and_then(|r| r.as_str())
            .unwrap_or("");
        let cache_hit = response
            .get("cache_hit")
            .and_then(|c| c.as_bool())
            .unwrap_or(false);

        crate::logging::info(&format!(
            "Auto-enrich: Mimir response status={} routed_to={} cache_hit={} elapsed={}ms",
            status, routed_to, cache_hit, elapsed_ms
        ));

        let formatted = format_mimir_enrich_response(&response);
        crate::logging::info(&format!(
            "Auto-enrich: got context ({} chars, {}ms)",
            formatted.len(),
            elapsed_ms
        ));
        crate::logging::debug(&format!(
            "Auto-enrich: formatted context preview: {}",
            {
                let preview: String = formatted.chars().take(300).collect();
                if formatted.len() > 300 {
                    format!("{}...", preview)
                } else {
                    preview
                }
            }
        ));

        Some(formatted)
    }

    /// Extract the latest user message for enrich_task.
    /// Returns the most recent user message so Mimir enriches based on the
    /// current request rather than the original task.
    /// Skips memory-injected <system-reminder> wrapper messages.
    fn extract_latest_user_message(&self) -> Option<String> {
        let latest: Option<String> = self
            .session
            .messages
            .iter()
            .rev()  // Start from the most recent
            .filter(|msg| matches!(msg.role, crate::message::Role::User))
            .filter_map(|msg| {
                msg.content.iter().find_map(|block| match block {
                    crate::message::ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
            })
            .find(|text| !text.trim().starts_with("<system-reminder>"))
            .map(|text| text.to_string());

        match &latest {
            Some(text) => {
                crate::logging::debug(&format!(
                    "Auto-enrich: selected latest user message ({} chars): {}",
                    text.len(),
                    text.chars().take(80).collect::<String>()
                ));
            }
            None => {
                crate::logging::debug("Auto-enrich: no suitable user message found");
            }
        }

        let text = latest?;
        if text.trim().is_empty() {
            crate::logging::debug("Auto-enrich: latest user message is empty after trim");
            return None;
        }

        // Cap context size to prevent massive Python bridge payloads
        if text.len() > MAX_ENRICH_TASK_CHARS {
            let capped: String = text.chars().take(MAX_ENRICH_TASK_CHARS).collect();
            logging::warn(&format!(
                "Enrich task context capped from {} to {} chars",
                text.len(),
                capped.len()
            ));
            Some(capped)
        } else {
            Some(text)
        }
    }

}

// --- Helper functions mirroring mimir.rs detection logic ---

/// Detect Mimir bridge path (mirrors mimir.rs logic)
fn detect_mimir_bridge(working_dir: Option<&Path>) -> Option<std::path::PathBuf> {
    use crate::logging;
    let start = working_dir.unwrap_or_else(|| Path::new("."));
    let mut current = start.to_path_buf();

    loop {
        let mimir_config = current.join(".mimir").join("config.json");
        if mimir_config.exists() {
            logging::debug(&format!("Auto-enrich: found .mimir/config.json at {}", current.display()));
            let candidates = [
                current.join("mimir_bridge.py"),
                current.join("scripts").join("mimir_bridge.py"),
            ];
            for candidate in &candidates {
                logging::debug(&format!("Auto-enrich: checking candidate bridge path: {}", candidate.display()));
                if candidate.exists() {
                    logging::debug(&format!("Auto-enrich: bridge script found at {}", candidate.display()));
                    return Some(candidate.clone());
                }
            }
            logging::debug("Auto-enrich: no bridge script found alongside .mimir/config.json");
            break;
        }
        if !current.pop() {
            logging::debug("Auto-enrich: reached filesystem root during bridge search");
            break;
        }
    }

    let home = dirs::home_dir()?;
    let fallbacks = [
        home.join("Documents").join("Mimir").join("scripts").join("mimir_bridge.py"),
        home.join("Documents").join("Mimir").join("mimir_bridge.py"),
        home.join(".local").join("share").join("mimir").join("mimir_bridge.py"),
    ];
    logging::debug(&format!("Auto-enrich: checking fallback locations from home={}", home.display()));
    for candidate in &fallbacks {
        logging::debug(&format!("Auto-enrich: checking fallback: {}", candidate.display()));
        if candidate.exists() {
            logging::debug(&format!("Auto-enrich: bridge found at fallback {}", candidate.display()));
            return Some(candidate.clone());
        }
    }
    logging::debug("Auto-enrich: no bridge script found in any location");
    None
}

/// Find Python executable (mirrors mimir.rs logic)
fn find_python() -> &'static str {
    static PYTHON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PYTHON.get_or_init(|| {
        for cmd in ["python3", "python"] {
            if std::process::Command::new(cmd)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return cmd.to_string();
            }
        }
        "python3".to_string()
    })
}

/// Format the enrich_task response for injection into context
fn format_mimir_enrich_response(response: &serde_json::Value) -> String {
    if let Some(error) = response.get("error").and_then(|e| e.as_str()) {
        crate::logging::warn(&format!("Auto-enrich: Mimir returned error: {}", error));
        return format!("[Mimir enrich_task error: {}]", error);
    }

    let status = response.get("status").and_then(|s| s.as_str()).unwrap_or("unknown");
    let context = response.get("context").and_then(|c| c.as_str()).unwrap_or("");
    let routed_to = response.get("routed_to").and_then(|r| r.as_str()).unwrap_or("");
    let elapsed = response.get("elapsed_ms").and_then(|e| e.as_u64()).unwrap_or(0);

    match status {
        "ok" => {
            let mut output = String::new();
            if !routed_to.is_empty() {
                output.push_str(&format!("[Mimir: enriched via {} in {}ms]\n\n", routed_to, elapsed));
            }
            output.push_str(context);
            output
        }
        "no_results" => {
            let suggestion = response.get("suggestion").and_then(|s| s.as_str()).unwrap_or("");
            format!("[Mimir: no project context found for this task. {}]", suggestion)
        }
        _ => {
            crate::logging::debug(&format!("Auto-enrich: Mimir returned status={}", status));
            format!("[Mimir: status={}] {}", status, context)
        }
    }
}
