use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;

/// Lists joined Matrix rooms with their display names, enabling room name resolution.
pub struct MatrixRoomsTool {
    homeserver: String,
    access_token: String,
    http_client: reqwest::Client,
}

impl MatrixRoomsTool {
    pub fn new(homeserver: String, access_token: String) -> Self {
        Self {
            homeserver: homeserver.trim_end_matches('/').to_string(),
            access_token,
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
impl Tool for MatrixRoomsTool {
    fn name(&self) -> &str {
        "matrix_rooms"
    }

    fn description(&self) -> &str {
        "List joined Matrix rooms with display names and aliases. Use to resolve a room name (e.g. \"General\") to its room ID, or to discover available rooms."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "string",
                    "description": "Optional case-insensitive substring to filter rooms by name or alias."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let filter = args
            .get("filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());

        // Step 1: Get joined rooms
        let joined_url = format!("{}/_matrix/client/v3/joined_rooms", self.homeserver);

        let resp = self
            .http_client
            .get(&joined_url)
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
            .map_err(|e| anyhow::anyhow!("Failed to parse joined_rooms response: {e}"))?;

        let room_ids = body
            .get("joined_rooms")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if room_ids.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No joined rooms found.".to_string(),
                error: None,
            });
        }

        // Step 2: Fetch display name and canonical alias for each room
        let mut rooms: Vec<RoomInfo> = Vec::with_capacity(room_ids.len());

        for room_val in &room_ids {
            let room_id = match room_val.as_str() {
                Some(id) => id,
                None => continue,
            };

            let encoded = encode_path_segment(room_id);

            // Fetch room name state event
            let name = self
                .fetch_state_event(&encoded, "m.room.name")
                .await
                .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(String::from));

            // Fetch canonical alias state event
            let alias = self
                .fetch_state_event(&encoded, "m.room.canonical_alias")
                .await
                .and_then(|v| v.get("alias").and_then(|a| a.as_str()).map(String::from));

            rooms.push(RoomInfo {
                room_id: room_id.to_string(),
                name,
                alias,
            });
        }

        // Step 3: Apply filter
        if let Some(ref f) = filter {
            rooms.retain(|r| {
                r.name.as_deref().unwrap_or("").to_lowercase().contains(f)
                    || r.alias.as_deref().unwrap_or("").to_lowercase().contains(f)
                    || r.room_id.to_lowercase().contains(f)
            });
        }

        if rooms.is_empty() {
            let msg = match filter {
                Some(f) => format!("No rooms matched filter \"{f}\"."),
                None => "No joined rooms found.".to_string(),
            };
            return Ok(ToolResult {
                success: true,
                output: msg,
                error: None,
            });
        }

        // Step 4: Format output
        let mut lines = Vec::with_capacity(rooms.len());
        for r in &rooms {
            let display = r.name.as_deref().unwrap_or("(unnamed)");
            let alias_str = r
                .alias
                .as_deref()
                .map(|a| format!("  alias: {a}"))
                .unwrap_or_default();
            lines.push(format!("- {display}\n  id: {}{alias_str}", r.room_id));
        }

        let output = format!(
            "Joined rooms ({} total):\n\n{}",
            rooms.len(),
            lines.join("\n")
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

struct RoomInfo {
    room_id: String,
    name: Option<String>,
    alias: Option<String>,
}

impl MatrixRoomsTool {
    async fn fetch_state_event(
        &self,
        encoded_room_id: &str,
        event_type: &str,
    ) -> Option<serde_json::Value> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/{}",
            self.homeserver, encoded_room_id, event_type
        );

        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        resp.json().await.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> MatrixRoomsTool {
        MatrixRoomsTool::new("https://matrix.example.com".into(), "test_token".into())
    }

    #[test]
    fn name_and_schema() {
        let tool = test_tool();
        assert_eq!(tool.name(), "matrix_rooms");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["filter"].is_object());
    }

    #[test]
    fn encode_room_id() {
        assert_eq!(
            encode_path_segment("!room:matrix.example.com"),
            "%21room%3Amatrix.example.com"
        );
    }

    #[tokio::test]
    async fn no_filter_passes() {
        // Network will fail but we verify no panic on empty args
        let tool = test_tool();
        let result = tool.execute(json!({})).await;
        // Expect a network error, not a logic error
        assert!(result.is_err() || !result.unwrap().success);
    }
}
