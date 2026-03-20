#!/usr/bin/env bash
# cron-bot-triage.sh — zero-token per-room triage poster
#
# For a given Matrix room:
#   1. Check idle state + dedup (via cron-bot-idle-check.py) — exit silently if not needed
#   2. Run `tk list` / `tk ready` — collect open tickets
#   3. Format a markdown summary
#   4. POST to Matrix CS API as cron-bot (no LLM)
#
# Usage:
#   ./services/cron-bot-triage.sh <room_id> [ticket_dir]
#
# Environment:
#   ZEROCLAW_CONFIG_DIR  — defaults to ~/.zeroclaw
#   CRON_BOT_CONFIG      — path to cron-bot.json (defaults to $ZEROCLAW_CONFIG_DIR/cron-bot.json)
#   DRY_RUN              — set to 1 to print the message without posting

set -euo pipefail

ROOM_ID="${1:-}"
if [[ -z "$ROOM_ID" ]]; then
    echo "Usage: $0 <room_id> [ticket_dir]" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZEROCLAW_CONFIG_DIR="${ZEROCLAW_CONFIG_DIR:-$HOME/.zeroclaw}"
CRON_BOT_CONFIG="${CRON_BOT_CONFIG:-$ZEROCLAW_CONFIG_DIR/cron-bot.json}"
TICKET_DIR="${2:-$(pwd)/.tickets}"
DRY_RUN="${DRY_RUN:-0}"

if [[ ! -f "$CRON_BOT_CONFIG" ]]; then
    echo "cron-bot config not found: $CRON_BOT_CONFIG" >&2
    exit 1
fi

HOMESERVER="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['homeserver'])")"
ACCESS_TOKEN="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['access_token'])")"

# ── Step 1: Idle + dedup check ─────────────────────────────────────────────
IDLE_CHECK="$SCRIPT_DIR/cron-bot-idle-check.py"
if [[ ! -f "$IDLE_CHECK" ]]; then
    echo "cron-bot-idle-check.py not found at $IDLE_CHECK" >&2
    exit 1
fi

idle_json="$(python3 "$IDLE_CHECK" "$ROOM_ID" --config "$CRON_BOT_CONFIG" 2>&1)" || true
should_post="$(echo "$idle_json" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if d.get('should_post') else 'no')" 2>/dev/null || echo "no")"

if [[ "$should_post" != "yes" ]]; then
    # Silent exit — room is not idle or cron-bot posted recently
    exit 0
fi

# ── Step 2: Collect ticket state ───────────────────────────────────────────
ticket_summary=""

if command -v tk &>/dev/null && [[ -d "$TICKET_DIR" ]]; then
    # Run tk in the ticket directory
    open_tickets="$(cd "$(dirname "$TICKET_DIR")" && tk list 2>/dev/null | grep -v '^$' | head -40 || echo "")"
    ready_tickets="$(cd "$(dirname "$TICKET_DIR")" && tk ready 2>/dev/null | grep -v '^$' | head -20 || echo "")"

    if [[ -n "$open_tickets" ]]; then
        ticket_summary="$open_tickets"
    fi
else
    # Fallback: parse .tickets/*.md frontmatter directly
    if [[ -d "$TICKET_DIR" ]]; then
        open_count=0
        open_lines=""
        while IFS= read -r -d '' f; do
            status="$(grep -m1 '^status:' "$f" 2>/dev/null | sed 's/status: *//' | tr -d '\r' || echo "")"
            if [[ "$status" != "done" && "$status" != "closed" && -n "$status" ]]; then
                title="$(grep -m1 '^title:' "$f" 2>/dev/null | sed 's/title: *//' | tr -d '\"' | tr -d '\r' || echo "$f")"
                priority="$(grep -m1 '^priority:' "$f" 2>/dev/null | sed 's/priority: *//' | tr -d '\r' || echo "")"
                open_lines="${open_lines}- [${priority}] ${title}
"
                (( open_count++ )) || true
            fi
        done < <(find "$TICKET_DIR" -name '*.md' -not -name 'README*' -print0 2>/dev/null)

        if [[ $open_count -gt 0 ]]; then
            ticket_summary="**Open Tickets (${open_count}):**
${open_lines}"
        fi
    fi
fi

if [[ -z "$ticket_summary" ]]; then
    # Nothing to report — exit silently
    exit 0
fi

# ── Step 3: Format message ─────────────────────────────────────────────────
now_utc="$(date -u '+%Y-%m-%d %H:%M UTC')"
message="**Triage Summary** — ${now_utc}

${ticket_summary}

_Next check in ~4h. Reply to discuss any item._"

# ── Step 4: Post to Matrix ─────────────────────────────────────────────────
encoded_room="$(python3 -c "import urllib.parse, sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$ROOM_ID")"
txn_id="cron-triage-$(date -u +%s)"
url="${HOMESERVER}/_matrix/client/v3/rooms/${encoded_room}/send/m.room.message/${txn_id}"

payload="$(python3 -c "
import json, sys
msg = sys.argv[1]
print(json.dumps({'msgtype': 'm.text', 'body': msg, 'format': 'org.matrix.custom.html', 'formatted_body': msg}))
" "$message")"

if [[ "$DRY_RUN" == "1" ]]; then
    echo "=== DRY RUN ==="
    echo "Room:    $ROOM_ID"
    echo "URL:     $url"
    echo "Message:"
    echo "$message"
    exit 0
fi

http_status="$(curl -s -o /tmp/cron-bot-post.json -w "%{http_code}" \
    -X PUT "$url" \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$payload")"

if [[ "$http_status" == "200" ]]; then
    event_id="$(python3 -c "import json; print(json.load(open('/tmp/cron-bot-post.json')).get('event_id','?'))" 2>/dev/null || echo "?")"
    echo "Posted triage to $ROOM_ID — event_id=$event_id"
else
    echo "Failed to post to $ROOM_ID — HTTP $http_status" >&2
    cat /tmp/cron-bot-post.json >&2 || true
    exit 1
fi
