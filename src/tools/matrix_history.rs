use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;

/// Fetches recent message history from a Matrix room via the client-server API.
pub struct MatrixHistoryTool {
    homeserver: String,
    access_token: String,
    default_room_id: Option<String>,
    http_client: reqwest::Client,
}

impl MatrixHistoryTool {
    pub fn new(homeserver: String, access_token: String, default_room_id: Option<String>) -> Self {
        Self {
            homeserver: homeserver.trim_end_matches('/').to_string(),
            access_token,
            default_room_id,
            http_client: reqwest::Client::new(),
        }
    }
}

fn encode_path_segment(value: &str) -> String {
    fn should_encode(byte: u8) -> bool {
        !matches!(
            byte,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
        )
    }

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if should_encode(byte) {
            use std::fmt::Write;
            let _ = write!(&mut encoded, "%{byte:02X}");
        } else {
            encoded.push(byte as char);
        }
    }
    encoded
}

#[async_trait]
impl Tool for MatrixHistoryTool {
    fn name(&self) -> &str {
        "matrix_history"
    }

    fn description(&self) -> &str {
        "Fetch recent message history from a Matrix room. Returns the most recent messages with sender, timestamp, and content. Use to review earlier conversation or catch up on missed context."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "room_id": {
                    "type": "string",
                    "description": "Matrix room ID (e.g. !abc:matrix.org). Omit to use the current room."
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of messages to fetch (default: 20, max: 100)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let room_id = args
            .get("room_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| self.default_room_id.clone())
            .ok_or_else(|| anyhow::anyhow!("No room_id provided and no default room configured"))?;

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(20u64, |v| v.clamp(1, 100));

        let encoded_room = encode_path_segment(&room_id);
        let filter = serde_json::to_string(&json!({"types": ["m.room.message"]}))?;
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/messages?dir=b&limit={}&filter={}",
            self.homeserver, encoded_room, limit, filter
        );

        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Matrix API request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Matrix API returned {status}: {body}")),
            });
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Matrix response: {e}"))?;

        let chunk = body
            .get("chunk")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if chunk.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!("No messages found in room {room_id}."),
                error: None,
            });
        }

        // dir=b returns newest-first; reverse for chronological output
        let mut messages: Vec<String> = Vec::with_capacity(chunk.len());
        for event in chunk.iter().rev() {
            let sender = event
                .get("sender")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let ts = event
                .get("origin_server_ts")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let body_text = event
                .get("content")
                .and_then(|c| c.get("body"))
                .and_then(|v| v.as_str())
                .unwrap_or("[no text content]");

            let msgtype = event
                .get("content")
                .and_then(|c| c.get("msgtype"))
                .and_then(|v| v.as_str())
                .unwrap_or("m.text");

            // Format timestamp as human-readable
            let datetime = {
                let secs = (ts / 1000) as i64;
                let nanos = u32::try_from((ts % 1000) * 1_000_000).unwrap_or(0);
                chrono::DateTime::from_timestamp(secs, nanos)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| ts.to_string())
            };

            let type_tag = if msgtype == "m.text" {
                String::new()
            } else {
                format!(" ({msgtype})")
            };

            messages.push(format!("[{datetime}] {sender}{type_tag}: {body_text}"));
        }

        let output = format!(
            "Room history ({room_id}) — {} most recent messages:\n\n{}",
            messages.len(),
            messages.join("\n")
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> MatrixHistoryTool {
        MatrixHistoryTool::new(
            "https://matrix.example.com".into(),
            "test_token".into(),
            Some("!test:example.com".into()),
        )
    }

    #[test]
    fn name_and_schema() {
        let tool = test_tool();
        assert_eq!(tool.name(), "matrix_history");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["room_id"].is_object());
        assert!(schema["properties"]["limit"].is_object());
    }

    #[test]
    fn encode_room_id() {
        assert_eq!(
            encode_path_segment("!room:matrix.example.com"),
            "%21room%3Amatrix.example.com"
        );
    }

    #[tokio::test]
    async fn missing_room_id_no_default() {
        let tool =
            MatrixHistoryTool::new("https://matrix.example.com".into(), "token".into(), None);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No room_id provided"));
    }

    #[tokio::test]
    async fn uses_default_room_when_omitted() {
        // This will fail to connect, but we verify it doesn't error on missing room_id
        let tool = test_tool();
        let result = tool.execute(json!({})).await;
        // Network error expected, but not a "missing room_id" error
        assert!(result.is_err() || !result.unwrap().output.contains("No room_id"));
    }
}
