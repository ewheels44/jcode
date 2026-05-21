//! Mimir knowledge base tool — thin CLI bridge to Mimir Python.
//!
//! Replaces the MCP server approach with direct subprocess calls.
//! No MCP handshake, no JSON-RPC, no process pool.
//!
//! Auto-detects `mimir_bridge.py` in the workspace by looking for
//! `.mimir/config.json` as a project marker.

use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;

const BRIDGE_TIMEOUT_SECS: u64 = 120;
const BRIDGE_SCRIPT_NAME: &str = "mimir_bridge.py";

/// Cached bridge path detection result per workspace.
static BRIDGE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Find the mimir_bridge.py script in the workspace.
///
/// Detection order:
/// 1. Walk up from working_dir looking for `.mimir/config.json`
/// 2. Check if `mimir_bridge.py` exists alongside it
/// 3. Check common locations: `~/Documents/Mimir/mimir_bridge.py`
fn detect_bridge_path(working_dir: Option<&Path>) -> Option<PathBuf> {
    // Walk up from working directory looking for .mimir/config.json
    let start = working_dir.unwrap_or_else(|| Path::new("."));
    let mut current = start.to_path_buf();

    loop {
        let mimir_config = current.join(".mimir").join("config.json");
        if mimir_config.exists() {
            // Check for bridge script in the same tree
            let candidates = [
                current.join(BRIDGE_SCRIPT_NAME),
                current.join("scripts").join(BRIDGE_SCRIPT_NAME),
            ];
            for candidate in &candidates {
                if candidate.exists() {
                    return Some(candidate.clone());
                }
            }
            // If .mimir/config.json exists but no bridge script found,
            // check if there's a Mimir installation referenced
            break;
        }

        if !current.pop() {
            break;
        }
    }

    // Fallback: check common Mimir install locations
    let home = dirs::home_dir()?;
    let fallbacks = [
        home.join("Documents").join("Mimir").join(BRIDGE_SCRIPT_NAME),
        home.join(".local").join("share").join("mimir").join(BRIDGE_SCRIPT_NAME),
    ];
    for candidate in &fallbacks {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }

    None
}

/// Get the cached bridge path (detected once per process).
fn get_bridge_path(working_dir: Option<&Path>) -> Option<&'static PathBuf> {
    BRIDGE_PATH.get_or_init(|| detect_bridge_path(working_dir)).as_ref()
}

/// Find Python executable.
fn find_python() -> &'static str {
    static PYTHON: OnceLock<String> = OnceLock::new();
    PYTHON.get_or_init(|| {
        // Try python3 first, then python
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

pub struct MimirTool;

impl MimirTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Deserialize)]
struct MimirInput {
    action: String,
    #[serde(default)]
    params: Option<Value>,
}

#[async_trait]
impl Tool for MimirTool {
    fn name(&self) -> &str {
        "mimir"
    }

    fn description(&self) -> &str {
        "Query the Mimir project knowledge base. Use enrich_task before coding tasks to get project context. Use search for semantic code search. Use sdk_cache_get for library documentation."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": [
                        "enrich_task",
                        "search",
                        "query",
                        "rag_workflow",
                        "knowledge_agent",
                        "reindex",
                        "remove_file",
                        "stats",
                        "task_health",
                        "sdk_cache_get",
                        "sdk_cache_list",
                        "cache_stats",
                        "cache_clear",
                        "cache_cleanup"
                    ],
                    "description": "Action to perform."
                },
                "params": {
                    "type": "object",
                    "description": "Action-specific parameters.",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "Task description for enrich_task."
                        },
                        "query": {
                            "type": "string",
                            "description": "Search/query text."
                        },
                        "question": {
                            "type": "string",
                            "description": "Question for query/knowledge_agent."
                        },
                        "top_k": {
                            "type": "integer",
                            "description": "Number of results (default: 5)."
                        },
                        "library": {
                            "type": "string",
                            "description": "Library name for sdk_cache_get."
                        },
                        "topic": {
                            "type": "string",
                            "description": "Topic for sdk_cache_get (default: general)."
                        },
                        "file_path": {
                            "type": "string",
                            "description": "File path for remove_file."
                        },
                        "response_shape": {
                            "type": "string",
                            "description": "JSON schema for rag_workflow structured output."
                        },
                        "force": {
                            "type": "boolean",
                            "description": "Force reindex (default: false)."
                        }
                    }
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: MimirInput = serde_json::from_value(input)?;

        // Find bridge script
        let bridge_path = match get_bridge_path(ctx.working_dir.as_deref()) {
            Some(path) => path.clone(),
            None => {
                return Ok(ToolOutput::new(
                    "Mimir not configured in this workspace.\n\n\
                     To set up Mimir:\n\
                     1. Clone Mimir: git clone <mimir-repo> ~/Documents/Mimir\n\
                     2. In your project: python ~/Documents/Mimir/mimir.py init\n\
                     3. This creates .mimir/config.json and sets up the knowledge base.",
                )
                .with_title("Mimir: Not configured"));
            }
        };

        // Build request JSON
        let request = json!({
            "action": params.action,
            "params": params.params.unwrap_or(json!({})),
        });

        let request_str = serde_json::to_string(&request)?;

        crate::logging::event_info(
            "MIMIR_TOOL",
            vec![
                ("phase", "start".to_string()),
                ("action", params.action.clone()),
                ("session_id", ctx.session_id.clone()),
                ("tool_call_id", ctx.tool_call_id.clone()),
                ("bridge", bridge_path.display().to_string()),
            ],
        );

        // Spawn Python bridge
        let python = find_python();
        let working_dir = ctx.working_dir.clone();

        let started = std::time::Instant::now();

        let mut cmd = Command::new(python);
        cmd.arg(&bridge_path);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Set environment
        if let Some(ref cwd) = working_dir {
            cmd.env("PROJECT_ROOT", cwd.display().to_string());
        }
        // Forward API key if available
        if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
            cmd.env("OPENROUTER_API_KEY", key);
        } else if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            cmd.env("OPENAI_API_KEY", key);
        }

        let mut child = cmd.spawn()?;

        // Write request to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(request_str.as_bytes()).await?;
            stdin.shutdown().await?;
        }

        // Wait with timeout
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(BRIDGE_TIMEOUT_SECS),
            child.wait_with_output(),
        )
        .await;

        let elapsed_ms = started.elapsed().as_millis();

        match output {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !output.status.success() && stdout.trim().is_empty() {
                    let error_msg = if stderr.trim().is_empty() {
                        format!("Bridge exited with code {}", output.status)
                    } else {
                        stderr.to_string()
                    };

                    crate::logging::event_warn(
                        "MIMIR_TOOL",
                        vec![
                            ("phase", "error".to_string()),
                            ("action", params.action.clone()),
                            ("error", error_msg.clone()),
                            ("elapsed_ms", elapsed_ms.to_string()),
                        ],
                    );

                    return Ok(ToolOutput::new(format!(
                        "Mimir bridge error: {}",
                        error_msg
                    ))
                    .with_title(format!("mimir:{}", params.action)));
                }

                // Parse JSON response
                let response: Value = match serde_json::from_str(&stdout) {
                    Ok(v) => v,
                    Err(e) => {
                        return Ok(ToolOutput::new(format!(
                            "Failed to parse Mimir response: {}\n\nRaw output:\n{}\n\nStderr:\n{}",
                            e, stdout, stderr
                        ))
                        .with_title(format!("mimir:{}", params.action)));
                    }
                };

                // Format response for the agent
                let formatted = format_mimir_response(&params.action, &response);

                crate::logging::event_info(
                    "MIMIR_TOOL",
                    vec![
                        ("phase", "done".to_string()),
                        ("action", params.action.clone()),
                        ("status", response.get("status").and_then(|s| s.as_str()).unwrap_or("unknown").to_string()),
                        ("elapsed_ms", elapsed_ms.to_string()),
                        ("output_bytes", formatted.len().to_string()),
                    ],
                );

                Ok(ToolOutput::new(formatted)
                    .with_title(format!("mimir:{}", params.action))
                    .with_metadata(response))
            }
            Ok(Err(e)) => {
                crate::logging::event_warn(
                    "MIMIR_TOOL",
                    vec![
                        ("phase", "spawn_error".to_string()),
                        ("action", params.action.clone()),
                        ("error", e.to_string()),
                        ("elapsed_ms", elapsed_ms.to_string()),
                    ],
                );
                Ok(ToolOutput::new(format!("Failed to run Mimir bridge: {}", e))
                    .with_title(format!("mimir:{}", params.action)))
            }
            Err(_) => {
                crate::logging::event_warn(
                    "MIMIR_TOOL",
                    vec![
                        ("phase", "timeout".to_string()),
                        ("action", params.action.clone()),
                        ("timeout_secs", BRIDGE_TIMEOUT_SECS.to_string()),
                        ("elapsed_ms", elapsed_ms.to_string()),
                    ],
                );
                Ok(ToolOutput::new(format!(
                    "Mimir bridge timed out after {}s. Try a simpler query or check if the index is built.",
                    BRIDGE_TIMEOUT_SECS
                ))
                .with_title(format!("mimir:{} (timeout)", params.action)))
            }
        }
    }
}

/// Format the Mimir bridge JSON response into a human-readable string for the agent.
fn format_mimir_response(action: &str, response: &Value) -> String {
    // Check for errors first
    if let Some(error) = response.get("error").and_then(|e| e.as_str()) {
        if response.get("status").and_then(|s| s.as_str()) == Some("no_index") {
            return format!(
                "No knowledge base index found. Run `mimir(action=\"reindex\")` to build it.\n\n{}",
                error
            );
        }
        return format!("Error: {}", error);
    }

    match action {
        "enrich_task" => format_enrich_task(response),
        "search" => format_search(response),
        "query" => format_query(response),
        "rag_workflow" | "knowledge_agent" => format_agent_response(response),
        "reindex" => format_simple(response, "Reindex"),
        "remove_file" => format_simple(response, "Remove file"),
        "stats" => format_stats(response),
        "task_health" => format_health(response),
        "sdk_cache_get" => format_sdk_docs(response),
        "sdk_cache_list" => format_sdk_list(response),
        "cache_stats" => format_cache_stats(response),
        "cache_clear" | "cache_cleanup" => format_simple(response, action),
        _ => {
            // Generic: return the context or answer field, or the whole response
            if let Some(context) = response.get("context").and_then(|c| c.as_str()) {
                if !context.is_empty() {
                    return context.to_string();
                }
            }
            if let Some(answer) = response.get("answer").and_then(|a| a.as_str()) {
                return answer.to_string();
            }
            if let Some(message) = response.get("message").and_then(|m| m.as_str()) {
                return message.to_string();
            }
            serde_json::to_string_pretty(response).unwrap_or_else(|_| response.to_string())
        }
    }
}

fn format_enrich_task(response: &Value) -> String {
    let status = response.get("status").and_then(|s| s.as_str()).unwrap_or("unknown");
    let context = response.get("context").and_then(|c| c.as_str()).unwrap_or("");
    let routed_to = response.get("routed_to").and_then(|r| r.as_str()).unwrap_or("");
    let elapsed = response.get("elapsed_ms").and_then(|e| e.as_u64()).unwrap_or(0);
    let cache_hit = response.get("cache_hit").and_then(|c| c.as_bool()).unwrap_or(false);

    match status {
        "ok" => {
            let mut output = String::new();
            if !routed_to.is_empty() {
                output.push_str(&format!("[Mimir: routed to {} ({}ms)", routed_to, elapsed));
                if cache_hit {
                    output.push_str(", cache hit");
                }
                output.push_str("]\n\n");
            }
            output.push_str(context);
            output
        }
        "no_results" => {
            let suggestion = response.get("suggestion").and_then(|s| s.as_str()).unwrap_or("");
            format!("No project context found for this task.\n{}", suggestion)
        }
        _ => context.to_string(),
    }
}

fn format_search(response: &Value) -> String {
    let results = match response.get("results").and_then(|r| r.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return "No search results found.".to_string(),
    };

    let mut output = String::new();
    for (i, result) in results.iter().enumerate() {
        let source = result.get("source").and_then(|s| s.as_str()).unwrap_or("unknown");
        let score = result.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
        let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");
        output.push_str(&format!("[{}] {} (score: {:.3})\n{}\n\n", i + 1, source, score, text));
    }
    output
}

fn format_query(response: &Value) -> String {
    response
        .get("answer")
        .and_then(|a| a.as_str())
        .unwrap_or("No answer returned.")
        .to_string()
}

fn format_agent_response(response: &Value) -> String {
    response
        .get("answer")
        .and_then(|a| a.as_str())
        .unwrap_or("No response returned.")
        .to_string()
}

fn format_simple(response: &Value, label: &str) -> String {
    response
        .get("message")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{} completed.", label))
}

fn format_stats(response: &Value) -> String {
    let stats = match response.get("stats") {
        Some(s) => s,
        None => return "No stats available.".to_string(),
    };

    let mut lines = vec![];
    if let Some(root) = stats.get("project_root").and_then(|r| r.as_str()) {
        lines.push(format!("Project: {}", root));
    }
    if let Some(has_index) = stats.get("has_index").and_then(|h| h.as_bool()) {
        lines.push(format!("Index: {}", if has_index { "yes" } else { "no" }));
    }
    if let Some(count) = stats.get("document_count") {
        lines.push(format!("Documents: {}", count));
    }
    if let Some(count) = stats.get("source_files").and_then(|c| c.as_u64()) {
        lines.push(format!("Source files: {}", count));
    }
    if let Some(dirs) = stats.get("code_dirs").and_then(|d| d.as_array()) {
        let dir_strs: Vec<&str> = dirs.iter().filter_map(|d| d.as_str()).collect();
        if !dir_strs.is_empty() {
            lines.push(format!("Code dirs: {}", dir_strs.join(", ")));
        }
    }
    lines.join("\n")
}

fn format_health(response: &Value) -> String {
    let health = match response.get("health") {
        Some(h) => h,
        None => return "No health data.".to_string(),
    };

    let mut lines = vec![];
    if let Some(enabled) = health.get("enabled").and_then(|e| e.as_bool()) {
        lines.push(format!("Router: {}", if enabled { "enabled" } else { "disabled" }));
    }
    if let Some(has_index) = health.get("has_index").and_then(|h| h.as_bool()) {
        lines.push(format!("Index: {}", if has_index { "ready" } else { "missing" }));
    }
    if let Some(top_k) = health.get("top_k").and_then(|t| t.as_u64()) {
        lines.push(format!("Top-K: {}", top_k));
    }
    if let Some(classifier) = health.get("neural_classifier_enabled").and_then(|c| c.as_bool()) {
        lines.push(format!("Neural classifier: {}", if classifier { "on" } else { "off" }));
    }
    lines.join("\n")
}

fn format_sdk_docs(response: &Value) -> String {
    let library = response.get("library").and_then(|l| l.as_str()).unwrap_or("?");
    let topic = response.get("topic").and_then(|t| t.as_str()).unwrap_or("general");
    let docs = response.get("docs").and_then(|d| d.as_str()).unwrap_or("No docs.");

    format!("## {} / {}\n\n{}", library, topic, docs)
}

fn format_sdk_list(response: &Value) -> String {
    let cached = match response.get("cached").and_then(|c| c.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return "No cached SDKs.".to_string(),
    };

    let mut lines = vec!["Cached SDKs:".to_string()];
    for entry in cached {
        let library = entry.get("library").and_then(|l| l.as_str()).unwrap_or("?");
        let fresh = entry.get("fresh").and_then(|f| f.as_bool()).unwrap_or(false);
        let topics = entry.get("topics").and_then(|t| t.as_array());
        let topic_str = topics
            .map(|ts| {
                ts.iter()
                    .filter_map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        lines.push(format!(
            "  - {} ({}) {}",
            library,
            if fresh { "fresh" } else { "stale" },
            if topic_str.is_empty() { String::new() } else { format!("[{}]", topic_str) }
        ));
    }
    lines.join("\n")
}

fn format_cache_stats(response: &Value) -> String {
    let stats = match response.get("cache_stats") {
        Some(s) => s,
        None => return "No cache stats.".to_string(),
    };

    serde_json::to_string_pretty(stats).unwrap_or_else(|_| "Cache stats unavailable.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name() {
        let tool = MimirTool::new();
        assert_eq!(tool.name(), "mimir");
    }

    #[test]
    fn test_tool_description() {
        let tool = MimirTool::new();
        assert!(tool.description().contains("knowledge base"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = MimirTool::new();
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["params"].is_object());

        // Verify all actions are in the enum
        let actions = schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap();
        assert!(actions.iter().any(|a| a == "enrich_task"));
        assert!(actions.iter().any(|a| a == "search"));
        assert!(actions.iter().any(|a| a == "sdk_cache_get"));
        assert_eq!(actions.len(), 14);
    }

    #[test]
    fn test_format_enrich_task_ok() {
        let response = json!({
            "status": "ok",
            "context": "Auth module uses JWT tokens.",
            "routed_to": "vector",
            "elapsed_ms": 150,
            "cache_hit": false
        });
        let formatted = format_enrich_task(&response);
        assert!(formatted.contains("Auth module uses JWT tokens"));
        assert!(formatted.contains("vector"));
    }

    #[test]
    fn test_format_search() {
        let response = json!({
            "status": "ok",
            "results": [
                {"source": "auth.rs", "score": 0.95, "text": "fn login()"},
                {"source": "middleware.rs", "score": 0.80, "text": "fn verify()"}
            ]
        });
        let formatted = format_search(&response);
        assert!(formatted.contains("auth.rs"));
        assert!(formatted.contains("middleware.rs"));
        assert!(formatted.contains("0.950"));
    }

    #[test]
    fn test_format_search_empty() {
        let response = json!({"status": "ok", "results": []});
        let formatted = format_search(&response);
        assert!(formatted.contains("No search results"));
    }

    #[test]
    fn test_format_error() {
        let response = json!({"error": "No index found", "status": "no_index"});
        let formatted = format_mimir_response("search", &response);
        assert!(formatted.contains("No knowledge base index found"));
        assert!(formatted.contains("reindex"));
    }

    #[test]
    fn test_format_stats() {
        let response = json!({
            "status": "ok",
            "stats": {
                "project_root": "/home/user/project",
                "has_index": true,
                "document_count": 42,
                "source_files": 150,
                "code_dirs": ["src", "tests"]
            }
        });
        let formatted = format_stats(&response);
        assert!(formatted.contains("42"));
        assert!(formatted.contains("150"));
        assert!(formatted.contains("src, tests"));
    }
}
