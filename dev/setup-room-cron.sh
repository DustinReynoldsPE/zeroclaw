#!/usr/bin/env bash
# setup-room-cron.sh — register per-room Matrix cron jobs with idle detection
#
# Each job runs on a schedule and calls services/cron-bot-triage.sh, which:
#   - Checks room idle state (no LLM)
#   - Posts a ticket triage summary as cron-bot if the room is idle (no LLM)
#   - Exits silently if the room is active or cron-bot posted recently
#
# Usage:
#   ZEROCLAW_CONFIG_DIR="$HOME/.zeroclaw" ./dev/setup-room-cron.sh [room_id ...]
#
# If no room IDs are given the script reads them from the config:
#   [channels_config.matrix.channel_workspaces] keys
#
# Environment:
#   ZEROCLAW_BIN          — path to zeroclaw binary (default: auto-detect)
#   ZEROCLAW_CONFIG_DIR   — config dir (default: ~/.zeroclaw)
#   ROOM_CRON_SCHEDULE    — cron expression (default: every 4 hours)
#   ROOM_CRON_TZ          — timezone (default: UTC)
#   TRIAGE_SCRIPT         — path to cron-bot-triage.sh (default: services/cron-bot-triage.sh)
#   TICKET_DIR            — .tickets dir passed to triage script (default: per-room workspace)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ZC="${ZEROCLAW_BIN:-$(command -v zeroclaw 2>/dev/null || echo "$REPO_ROOT/target/release/zeroclaw")}"
SCHEDULE="${ROOM_CRON_SCHEDULE:-0 */4 * * *}"   # every 4 hours by default
TZ="${ROOM_CRON_TZ:-UTC}"
ZEROCLAW_CONFIG_DIR="${ZEROCLAW_CONFIG_DIR:-$HOME/.zeroclaw}"
TRIAGE_SCRIPT="${TRIAGE_SCRIPT:-$REPO_ROOT/services/cron-bot-triage.sh}"

if [[ ! -x "$ZC" ]]; then
    echo "zeroclaw binary not found at '$ZC'" >&2
    echo "Set ZEROCLAW_BIN or build with: cargo build --release" >&2
    exit 1
fi

if [[ ! -f "$TRIAGE_SCRIPT" ]]; then
    echo "cron-bot-triage.sh not found at '$TRIAGE_SCRIPT'" >&2
    exit 1
fi

# Collect room IDs from arguments or config
room_ids=("$@")

if [[ ${#room_ids[@]} -eq 0 ]]; then
    config="$ZEROCLAW_CONFIG_DIR/config.toml"
    if [[ ! -f "$config" ]]; then
        echo "No room IDs given and config not found at $config" >&2
        exit 1
    fi
    # Extract channel_workspaces keys (Matrix room IDs)
    mapfile -t room_ids < <(
        grep -E '^\s*"!.*:' "$config" \
        | grep -oE '"![^"]*"' \
        | tr -d '"' \
        | sort -u
    )
    if [[ ${#room_ids[@]} -eq 0 ]]; then
        echo "No Matrix room IDs found in $config" >&2
        exit 1
    fi
fi

# Map room_id -> workspace dir from config (for ticket discovery)
declare -A room_workspaces
while IFS= read -r line; do
    room="$(echo "$line" | grep -oE '"![^"]*"' | tr -d '"')"
    workspace="$(echo "$line" | grep -oP '=\s*"\K[^"]+' | tail -1)"
    if [[ -n "$room" && -n "$workspace" ]]; then
        room_workspaces["$room"]="$workspace"
    fi
done < <(grep -E '^\s*"!.*:' "$ZEROCLAW_CONFIG_DIR/config.toml" 2>/dev/null || true)

echo "Registering per-room shell cron jobs (schedule: $SCHEDULE $TZ)"
echo "  Triage script: $TRIAGE_SCRIPT"
echo ""

registered=0
skipped=0

for room_id in "${room_ids[@]}"; do
    # Resolve ticket dir: workspace/.tickets or $REPO_ROOT/.tickets as fallback
    workspace="${room_workspaces[$room_id]:-}"
    if [[ -n "$workspace" && -d "$workspace/.tickets" ]]; then
        ticket_dir="$workspace/.tickets"
    elif [[ -d "$REPO_ROOT/.tickets" ]]; then
        ticket_dir="$REPO_ROOT/.tickets"
    else
        ticket_dir=""
    fi

    cmd="bash $TRIAGE_SCRIPT $room_id"
    if [[ -n "$ticket_dir" ]]; then
        cmd="$cmd $ticket_dir"
    fi

    echo "  Room: $room_id"
    [[ -n "$ticket_dir" ]] && echo "    tickets: $ticket_dir"

    ZEROCLAW_CONFIG_DIR="$ZEROCLAW_CONFIG_DIR" "$ZC" cron add "$SCHEDULE" "$cmd" --tz "$TZ"
    echo "  -> registered (shell, zero-token)"
    (( registered++ )) || true
done

echo ""
echo "Done. $registered job(s) registered."
echo "Verify with: ZEROCLAW_CONFIG_DIR=$ZEROCLAW_CONFIG_DIR $ZC cron list"
