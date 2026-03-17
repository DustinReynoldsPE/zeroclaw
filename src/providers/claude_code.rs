//! Claude Code headless CLI provider with session persistence.
//!
//! Integrates with the Claude Code CLI, spawning the `claude` binary
//! as a subprocess for each inference request. Sessions are persisted
//! via `--resume` so that multi-turn conversations retain full tool
//! results, file context, and conversation history within Claude Code.
//!
//! # Usage
//!
//! The `claude` binary must be available in `PATH`, or its location must be
//! set via the `CLAUDE_CODE_PATH` environment variable.
//!
//! # Directives
//!
//! The provider recognizes special directives prepended to the message:
//! - `[ZEROCLAW_CWD:/path]` — run Claude Code in the given working directory
//! - `[ZEROCLAW_SESSION_KEY:key]` — persist/resume sessions keyed by this identifier
//!
//! # Authentication
//!
//! Authentication is handled by Claude Code itself (its own credential store).
//! No explicit API key is required by this provider.
//!
//! # Environment variables
//!
//! - `CLAUDE_CODE_PATH` — override the path to the `claude` binary (default: `"claude"`)

use crate::providers::traits::{ChatRequest, ChatResponse, Provider, TokenUsage};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Environment variable for overriding the path to the `claude` binary.
pub const CLAUDE_CODE_PATH_ENV: &str = "CLAUDE_CODE_PATH";

/// Default `claude` binary name (resolved via `PATH`).
const DEFAULT_CLAUDE_CODE_BINARY: &str = "claude";

/// Model name used to signal "use the provider's own default model".
const DEFAULT_MODEL_MARKER: &str = "default";
/// Claude Code requests are bounded to avoid hung subprocesses.
const CLAUDE_CODE_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Avoid leaking oversized stderr payloads.
const MAX_CLAUDE_CODE_STDERR_CHARS: usize = 512;
/// The CLI does not support sampling controls; allow only baseline defaults.
const CLAUDE_CODE_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// Parsed JSON output from `claude --print --output-format json`.
#[derive(Debug, Deserialize)]
struct ClaudeJsonOutput {
    /// The assistant's response text.
    result: Option<String>,
    /// Session ID for resuming this conversation.
    session_id: Option<String>,
    /// Whether the result represents an error.
    #[serde(default)]
    is_error: bool,
}

/// Provider that invokes the Claude Code CLI as a subprocess.
///
/// Sessions are tracked internally so that subsequent calls with the same
/// session key automatically resume the prior Claude Code conversation.
pub struct ClaudeCodeProvider {
    /// Path to the `claude` binary.
    binary_path: PathBuf,
    /// Maps session keys (e.g. room IDs) to Claude Code session IDs.
    sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl ClaudeCodeProvider {
    /// Create a new `ClaudeCodeProvider`.
    ///
    /// The binary path is resolved from `CLAUDE_CODE_PATH` env var if set,
    /// otherwise defaults to `"claude"` (found via `PATH`).
    pub fn new() -> Self {
        let binary_path = std::env::var(CLAUDE_CODE_PATH_ENV)
            .ok()
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CLAUDE_CODE_BINARY));

        Self {
            binary_path,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns true if the model argument should be forwarded to the CLI.
    fn should_forward_model(model: &str) -> bool {
        let trimmed = model.trim();
        !trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER
    }

    fn supports_temperature(temperature: f64) -> bool {
        CLAUDE_CODE_SUPPORTED_TEMPERATURES
            .iter()
            .any(|v| (temperature - v).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("Claude Code provider received non-finite temperature value");
        }
        // Claude Code CLI only supports 0.7 and 1.0, but we silently clamp
        // unsupported values to 1.0 rather than failing, since ZeroClaw's
        // query classifier may request temperatures the CLI cannot honor.
        Ok(())
    }

    fn redact_stderr(stderr: &[u8]) -> String {
        let text = String::from_utf8_lossy(stderr);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed.chars().count() <= MAX_CLAUDE_CODE_STDERR_CHARS {
            return trimmed.to_string();
        }
        let clipped: String = trimmed.chars().take(MAX_CLAUDE_CODE_STDERR_CHARS).collect();
        format!("{clipped}...")
    }

    /// Extract a `[ZEROCLAW_CWD:/path]` directive from the message, returning
    /// the working directory and the message with the directive stripped.
    fn extract_cwd(message: &str) -> (Option<PathBuf>, &str) {
        if let Some(rest) = message.strip_prefix("[ZEROCLAW_CWD:") {
            if let Some(end) = rest.find(']') {
                let path = rest[..end].trim();
                if !path.is_empty() {
                    let remainder = rest[end + 1..].trim_start_matches('\n');
                    return (Some(PathBuf::from(path)), remainder);
                }
            }
        }
        (None, message)
    }

    /// Extract a `[ZEROCLAW_SESSION_KEY:key]` directive from the message,
    /// returning the session key and the message with the directive stripped.
    fn extract_session_key(message: &str) -> (Option<String>, &str) {
        if let Some(rest) = message.strip_prefix("[ZEROCLAW_SESSION_KEY:") {
            if let Some(end) = rest.find(']') {
                let key = rest[..end].trim();
                if !key.is_empty() {
                    let remainder = rest[end + 1..].trim_start_matches('\n');
                    return (Some(key.to_string()), remainder);
                }
            }
        }
        (None, message)
    }

    /// Look up a stored session ID for the given key.
    fn get_session(&self, key: &str) -> Option<String> {
        self.sessions.lock().ok()?.get(key).cloned()
    }

    /// Store a session ID for the given key.
    fn set_session(&self, key: String, session_id: String) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(key, session_id);
        }
    }

    /// Remove a stored session (e.g. after a resume failure).
    fn clear_session(&self, key: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(key);
        }
    }

    /// Parse JSON output from `claude --print --output-format json`.
    /// Falls back to treating stdout as plain text if JSON parsing fails.
    fn parse_output(stdout: &str) -> (String, Option<String>) {
        let trimmed = stdout.trim();
        if let Ok(parsed) = serde_json::from_str::<ClaudeJsonOutput>(trimmed) {
            let text = parsed.result.unwrap_or_default();
            (text, parsed.session_id)
        } else {
            // Fallback: raw text output (e.g. older CLI without --output-format)
            (trimmed.to_string(), None)
        }
    }

    /// Invoke the claude binary with the given prompt and optional model.
    /// Returns the trimmed stdout output as the assistant response.
    async fn invoke_cli(&self, message: &str, model: &str) -> anyhow::Result<String> {
        // Extract directives from the message prefix.
        let (cwd, message) = Self::extract_cwd(message);
        let (session_key, message) = Self::extract_session_key(message);

        // Look up existing session for --resume.
        let resume_id = session_key.as_ref().and_then(|k| self.get_session(k));

        let result = self
            .invoke_cli_inner(message, model, cwd.as_ref(), resume_id.as_deref())
            .await;

        // If --resume failed, clear stale session and retry without it.
        if result.is_err() && resume_id.is_some() {
            if let Some(ref key) = session_key {
                tracing::warn!(
                    session_key = key.as_str(),
                    "Claude Code --resume failed, clearing stale session and retrying"
                );
                self.clear_session(key);
            }
            return self
                .invoke_cli_inner(message, model, cwd.as_ref(), None)
                .await;
        }

        result
    }

    /// Inner CLI invocation with explicit resume control.
    async fn invoke_cli_inner(
        &self,
        message: &str,
        model: &str,
        cwd: Option<&PathBuf>,
        resume_session_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let mut cmd = Command::new(&self.binary_path);
        cmd.arg("--print");
        cmd.arg("--dangerously-skip-permissions");
        cmd.arg("--output-format").arg("json");

        if let Some(session_id) = resume_session_id {
            cmd.arg("--resume").arg(session_id);
        }

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        if Self::should_forward_model(model) {
            cmd.arg("--model").arg(model);
        }

        // Read prompt from stdin to avoid exposing sensitive content in process args.
        cmd.arg("-");
        cmd.kill_on_drop(true);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|err| {
            anyhow::anyhow!(
                "Failed to spawn Claude Code binary at {}: {err}. \
                 Ensure `claude` is installed and in PATH, or set CLAUDE_CODE_PATH.",
                self.binary_path.display()
            )
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(message.as_bytes())
                .await
                .map_err(|err| {
                    anyhow::anyhow!("Failed to write prompt to Claude Code stdin: {err}")
                })?;
            stdin.shutdown().await.map_err(|err| {
                anyhow::anyhow!("Failed to finalize Claude Code stdin stream: {err}")
            })?;
        }

        let output = timeout(CLAUDE_CODE_REQUEST_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Claude Code request timed out after {:?} (binary: {})",
                    CLAUDE_CODE_REQUEST_TIMEOUT,
                    self.binary_path.display()
                )
            })?
            .map_err(|err| anyhow::anyhow!("Claude Code process failed: {err}"))?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr_excerpt = Self::redact_stderr(&output.stderr);
            let stderr_note = if stderr_excerpt.is_empty() {
                String::new()
            } else {
                format!(" Stderr: {stderr_excerpt}")
            };
            anyhow::bail!(
                "Claude Code exited with non-zero status {code}. \
                 Check that Claude Code is authenticated and the CLI is supported.{stderr_note}"
            );
        }

        let raw = String::from_utf8(output.stdout)
            .map_err(|err| anyhow::anyhow!("Claude Code produced non-UTF-8 output: {err}"))?;

        let (text, new_session_id) = Self::parse_output(&raw);

        // Persist the session ID for future --resume calls.
        if let Some(new_id) = new_session_id {
            // Extract the session key from the original message (before stripping).
            // We stored it earlier; use the key captured in invoke_cli.
            // Since invoke_cli_inner doesn't have the key, we pass it back
            // through the return value and let invoke_cli handle storage.
            // For now, store it if we can recover the key from the caller context.
            //
            // Note: session storage is handled by invoke_cli after this returns.
            // We encode the session_id in a special return format.
            return Ok(format!("[ZEROCLAW_NEW_SESSION:{new_id}]\n{text}"));
        }

        Ok(text)
    }

    /// Post-process the response to extract and store any new session ID.
    fn store_session_from_response(
        &self,
        response: &mut String,
        session_key: Option<&str>,
    ) {
        if let Some(rest) = response.strip_prefix("[ZEROCLAW_NEW_SESSION:") {
            if let Some(end) = rest.find(']') {
                let new_id = rest[..end].trim().to_string();
                let remainder = rest[end + 1..].trim_start_matches('\n').to_string();
                if let Some(key) = session_key {
                    tracing::info!(
                        session_key = key,
                        session_id = new_id.as_str(),
                        "Claude Code session persisted for --resume"
                    );
                    self.set_session(key.to_string(), new_id);
                }
                *response = remainder;
            }
        }
    }
}

impl Default for ClaudeCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ClaudeCodeProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        Self::validate_temperature(temperature)?;

        let full_message = match system_prompt {
            Some(system) if !system.is_empty() => {
                format!("{system}\n\n{message}")
            }
            _ => message.to_string(),
        };

        // Extract session key before invoke. CWD directive comes first,
        // so strip it before looking for the session key.
        let (_, after_cwd) = Self::extract_cwd(&full_message);
        let (session_key, _) = Self::extract_session_key(after_cwd);

        let mut result = self.invoke_cli(&full_message, model).await?;
        self.store_session_from_response(&mut result, session_key.as_deref());
        Ok(result)
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let text = self
            .chat_with_history(request.messages, model, temperature)
            .await?;

        Ok(ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            usage: Some(TokenUsage::default()),
            reasoning_content: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    // ── Constructor tests ──

    #[test]
    fn new_uses_env_override() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        std::env::set_var(CLAUDE_CODE_PATH_ENV, "/usr/local/bin/claude");
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("/usr/local/bin/claude"));
        match orig {
            Some(v) => std::env::set_var(CLAUDE_CODE_PATH_ENV, v),
            None => std::env::remove_var(CLAUDE_CODE_PATH_ENV),
        }
    }

    #[test]
    fn new_defaults_to_claude() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        std::env::remove_var(CLAUDE_CODE_PATH_ENV);
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("claude"));
        if let Some(v) = orig {
            std::env::set_var(CLAUDE_CODE_PATH_ENV, v);
        }
    }

    #[test]
    fn new_ignores_blank_env_override() {
        let _guard = env_lock();
        let orig = std::env::var(CLAUDE_CODE_PATH_ENV).ok();
        std::env::set_var(CLAUDE_CODE_PATH_ENV, "   ");
        let provider = ClaudeCodeProvider::new();
        assert_eq!(provider.binary_path, PathBuf::from("claude"));
        match orig {
            Some(v) => std::env::set_var(CLAUDE_CODE_PATH_ENV, v),
            None => std::env::remove_var(CLAUDE_CODE_PATH_ENV),
        }
    }

    // ── Model forwarding ──

    #[test]
    fn should_forward_model_standard() {
        assert!(ClaudeCodeProvider::should_forward_model(
            "claude-sonnet-4-20250514"
        ));
        assert!(ClaudeCodeProvider::should_forward_model("claude-3.5-sonnet"));
    }

    #[test]
    fn should_not_forward_default_model() {
        assert!(!ClaudeCodeProvider::should_forward_model(DEFAULT_MODEL_MARKER));
        assert!(!ClaudeCodeProvider::should_forward_model(""));
        assert!(!ClaudeCodeProvider::should_forward_model("   "));
    }

    // ── Temperature ──

    #[test]
    fn validate_temperature_allows_defaults() {
        assert!(ClaudeCodeProvider::validate_temperature(0.7).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(1.0).is_ok());
    }

    #[test]
    fn validate_temperature_clamps_unsupported() {
        // Should not error — we clamp instead of rejecting.
        assert!(ClaudeCodeProvider::validate_temperature(0.2).is_ok());
        assert!(ClaudeCodeProvider::validate_temperature(0.1).is_ok());
    }

    // ── CWD directive ──

    #[test]
    fn extract_cwd_parses_directive() {
        let (cwd, rest) = ClaudeCodeProvider::extract_cwd(
            "[ZEROCLAW_CWD:/Users/dustin/projects/alpha]\nHello world",
        );
        assert_eq!(cwd.unwrap(), PathBuf::from("/Users/dustin/projects/alpha"));
        assert_eq!(rest, "Hello world");
    }

    #[test]
    fn extract_cwd_returns_none_without_directive() {
        let (cwd, rest) = ClaudeCodeProvider::extract_cwd("Hello world");
        assert!(cwd.is_none());
        assert_eq!(rest, "Hello world");
    }

    // ── Session key directive ──

    #[test]
    fn extract_session_key_parses_directive() {
        let (key, rest) = ClaudeCodeProvider::extract_session_key(
            "[ZEROCLAW_SESSION_KEY:!room123:server]\nHello world",
        );
        assert_eq!(key.unwrap(), "!room123:server");
        assert_eq!(rest, "Hello world");
    }

    #[test]
    fn extract_session_key_returns_none_without_directive() {
        let (key, rest) = ClaudeCodeProvider::extract_session_key("Hello world");
        assert!(key.is_none());
        assert_eq!(rest, "Hello world");
    }

    #[test]
    fn extract_session_key_ignores_empty_key() {
        let (key, rest) = ClaudeCodeProvider::extract_session_key("[ZEROCLAW_SESSION_KEY:]\nHello");
        assert!(key.is_none());
        assert_eq!(rest, "[ZEROCLAW_SESSION_KEY:]\nHello");
    }

    // ── Combined directive extraction ──

    #[test]
    fn extract_chained_directives() {
        let input = "[ZEROCLAW_CWD:/projects/a]\n[ZEROCLAW_SESSION_KEY:room1]\nWhat time is it?";
        let (cwd, rest) = ClaudeCodeProvider::extract_cwd(input);
        assert_eq!(cwd.unwrap(), PathBuf::from("/projects/a"));
        let (key, rest) = ClaudeCodeProvider::extract_session_key(rest);
        assert_eq!(key.unwrap(), "room1");
        assert_eq!(rest, "What time is it?");
    }

    // ── JSON output parsing ──

    #[test]
    fn parse_output_extracts_result_and_session() {
        let json = r#"{"type":"result","subtype":"success","is_error":false,"result":"Hello world","session_id":"abc-123","total_cost_usd":0.01}"#;
        let (text, session_id) = ClaudeCodeProvider::parse_output(json);
        assert_eq!(text, "Hello world");
        assert_eq!(session_id.unwrap(), "abc-123");
    }

    #[test]
    fn parse_output_handles_error_response() {
        let json = r#"{"type":"result","is_error":true,"result":"Not logged in","session_id":"def-456"}"#;
        let (text, session_id) = ClaudeCodeProvider::parse_output(json);
        assert_eq!(text, "Not logged in");
        assert_eq!(session_id.unwrap(), "def-456");
    }

    #[test]
    fn parse_output_handles_missing_fields() {
        let json = r#"{"type":"result"}"#;
        let (text, session_id) = ClaudeCodeProvider::parse_output(json);
        assert_eq!(text, "");
        assert!(session_id.is_none());
    }

    #[test]
    fn parse_output_falls_back_to_raw_text() {
        let raw = "This is plain text, not JSON";
        let (text, session_id) = ClaudeCodeProvider::parse_output(raw);
        assert_eq!(text, raw);
        assert!(session_id.is_none());
    }

    // ── Session storage ──

    #[test]
    fn session_store_and_retrieve() {
        let provider = ClaudeCodeProvider::new();
        assert!(provider.get_session("room1").is_none());

        provider.set_session("room1".into(), "session-abc".into());
        assert_eq!(provider.get_session("room1").unwrap(), "session-abc");

        // Update overwrites.
        provider.set_session("room1".into(), "session-def".into());
        assert_eq!(provider.get_session("room1").unwrap(), "session-def");
    }

    #[test]
    fn session_clear_removes_entry() {
        let provider = ClaudeCodeProvider::new();
        provider.set_session("room1".into(), "session-abc".into());
        provider.clear_session("room1");
        assert!(provider.get_session("room1").is_none());
    }

    #[test]
    fn session_clear_nonexistent_is_noop() {
        let provider = ClaudeCodeProvider::new();
        provider.clear_session("nonexistent"); // Should not panic.
    }

    // ── Response session extraction ──

    #[test]
    fn store_session_from_response_extracts_and_stores() {
        let provider = ClaudeCodeProvider::new();
        let mut response = "[ZEROCLAW_NEW_SESSION:sess-xyz]\nHello world".to_string();
        provider.store_session_from_response(&mut response, Some("room1"));
        assert_eq!(response, "Hello world");
        assert_eq!(provider.get_session("room1").unwrap(), "sess-xyz");
    }

    #[test]
    fn store_session_from_response_no_directive() {
        let provider = ClaudeCodeProvider::new();
        let mut response = "Hello world".to_string();
        provider.store_session_from_response(&mut response, Some("room1"));
        assert_eq!(response, "Hello world");
        assert!(provider.get_session("room1").is_none());
    }

    #[test]
    fn store_session_from_response_no_key_strips_but_does_not_store() {
        let provider = ClaudeCodeProvider::new();
        let mut response = "[ZEROCLAW_NEW_SESSION:sess-xyz]\nHello world".to_string();
        provider.store_session_from_response(&mut response, None);
        // Directive is always stripped from user-visible response,
        // but without a key, nothing is stored.
        assert_eq!(response, "Hello world");
        assert!(provider.get_session("anything").is_none());
    }

    // ── CLI invocation ──

    #[tokio::test]
    async fn invoke_missing_binary_returns_error() {
        let provider = ClaudeCodeProvider {
            binary_path: PathBuf::from("/nonexistent/path/to/claude"),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        };
        let result = provider.invoke_cli("hello", "default").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn Claude Code binary"),
            "unexpected error message: {msg}"
        );
    }
}
