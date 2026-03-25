#!/usr/bin/env bash
# add-matrix-room.sh — Create a Matrix room, invite the bot, and print config entries.
#
# Usage:
#   ./scripts/add-matrix-room.sh <room-name> <workspace-path> [tmux-target]
#
# Example:
#   ./scripts/add-matrix-room.sh workqueue ~/code/workqueue main:workqueue
#
# Requires: admin credentials (prompts or reads from env).

set -euo pipefail

ROOM_NAME="${1:?Usage: $0 <room-name> <workspace-path> [tmux-target]}"
WORKSPACE="${2:?Usage: $0 <room-name> <workspace-path> [tmux-target]}"
TMUX_TARGET="${3:-}"

# Read Matrix config from zeroclaw config
CONFIG="${HOME}/.zeroclaw/config.toml"
HS=$(grep '^homeserver' "$CONFIG" | head -1 | sed 's/.*= *"//;s/"//')
BOT_USER=$(grep '^user_id' "$CONFIG" | head -1 | sed 's/.*= *"//;s/"//')
SERVER_NAME="${BOT_USER##*:}"

# Admin credentials
if [[ -z "${ADMIN_USER:-}" ]]; then
    read -rp "Admin username (local part): " ADMIN_USER
fi
if [[ -z "${ADMIN_PASSWORD:-}" ]]; then
    read -rsp "Admin password: " ADMIN_PASSWORD; echo
fi

ADMIN_MXID="@${ADMIN_USER}:${SERVER_NAME}"

# Login as admin
echo "Logging in as ${ADMIN_MXID}..."
ADMIN_TOKEN=$(curl -sf -X POST "${HS}/_matrix/client/v3/login" \
    -H "Content-Type: application/json" \
    -d "{\"type\":\"m.login.password\",\"user\":\"${ADMIN_USER}\",\"password\":\"${ADMIN_PASSWORD}\"}" \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['access_token'])")

# Create room
echo "Creating room '${ROOM_NAME}'..."
ROOM_ID=$(curl -sf -X POST "${HS}/_matrix/client/v3/createRoom" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer ${ADMIN_TOKEN}" \
    -d "{\"name\":\"${ROOM_NAME}\",\"room_alias_name\":\"${ROOM_NAME}\",\"preset\":\"private_chat\",\"visibility\":\"private\"}" \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['room_id'])")

echo "  Room ID: ${ROOM_ID}"

# Invite bot
echo "Inviting ${BOT_USER}..."
ENCODED_ROOM=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1],safe=''))" "${ROOM_ID}")
curl -sf -X POST "${HS}/_matrix/client/v3/rooms/${ENCODED_ROOM}/invite" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer ${ADMIN_TOKEN}" \
    -d "{\"user_id\":\"${BOT_USER}\"}" >/dev/null

# Bot joins (using bot token from config)
BOT_TOKEN=$(grep '^access_token' "$CONFIG" | head -1 | sed 's/.*= *"//;s/"//')
curl -sf -X POST "${HS}/_matrix/client/v3/join/${ENCODED_ROOM}" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer ${BOT_TOKEN}" \
    -d '{}' >/dev/null

echo "  Bot joined."

# Resolve workspace to absolute path
WORKSPACE="$(realpath "${WORKSPACE}")"

# Output config
echo ""
echo "=== Add to ~/.zeroclaw/config.toml ==="
echo ""
echo "[channel_workspaces]"
echo "\"${ROOM_ID}\" = \"${WORKSPACE}\""
echo ""
if [[ -n "${TMUX_TARGET}" ]]; then
    echo "[tmux_targets]"
    echo "\"${ROOM_ID}\" = \"${TMUX_TARGET}\""
    echo ""
fi
echo "Then restart zeroclaw: sudo systemctl restart zeroclaw"
