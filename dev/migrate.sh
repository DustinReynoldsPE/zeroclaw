#!/usr/bin/env bash
# migrate.sh — Migrate a running ZeroClaw instance to a remote host.
#
# Syncs ~/.zeroclaw config to the remote, installs cron jobs, and starts
# the daemon in a named tmux window. Safe to re-run; syncs incrementally
# and restarts the daemon cleanly.
#
# Prerequisites on the remote:
#   - zeroclaw binary at ~/code/zeroclaw/target/release/zeroclaw
#   - tmux installed
#   - SSH key auth (no interactive password prompts)
#
# Usage:
#   ./dev/migrate.sh <[user@]host> [tmux-target]
#
# Examples:
#   ./dev/migrate.sh hpllm.local
#   ./dev/migrate.sh hpllm.local main:zeroclaw
#   ./dev/migrate.sh user@192.168.1.50 dev:agent

set -euo pipefail

HOST="${1:-}"
TMUX_TARGET="${2:-main:zeroclaw}"

if [[ -z "$HOST" ]]; then
    echo "Usage: $0 [user@]host [tmux-session:window]" >&2
    echo "  e.g. $0 hpllm.local main:zeroclaw" >&2
    exit 1
fi

SESSION="${TMUX_TARGET%%:*}"
WINDOW="${TMUX_TARGET##*:}"

log() { echo "[migrate] $*"; }

# ── 1. Sync ~/.zeroclaw config ─────────────────────────────────────────────
log "Syncing ~/.zeroclaw → ${HOST}:~/.zeroclaw ..."
rsync -av --delete \
    --exclude='tts_cache/' \
    --exclude='captures/' \
    --exclude='recordings/' \
    --exclude='claude_code_sessions.json' \
    ~/.zeroclaw/ "${HOST}:~/.zeroclaw/"
log "Config sync complete."

# ── 1b. Rewrite active_workspace.toml with remote $HOME ───────────────────
log "Fixing active_workspace.toml on ${HOST} ..."
ssh "$HOST" 'echo "config_dir = \"$HOME/.zeroclaw\"" > ~/.zeroclaw/active_workspace.toml'

# ── 2. Install cron jobs on remote ────────────────────────────────────────
log "Installing cron jobs on ${HOST} ..."
ssh "$HOST" "bash ~/code/zeroclaw/dev/install-cron.sh"

# ── 3. Start daemon in tmux window ────────────────────────────────────────
log "Starting daemon in tmux '${TMUX_TARGET}' on ${HOST} ..."
ssh "$HOST" bash -s "$SESSION" "$WINDOW" <<'REMOTE'
set -euo pipefail
SESSION="$1"
WINDOW="$2"
TARGET="${SESSION}:${WINDOW}"

# Stop any existing daemon
pkill -f 'zeroclaw daemon' 2>/dev/null && sleep 1 || true

# Ensure the tmux session exists
if ! tmux has-session -t "$SESSION" 2>/dev/null; then
    tmux new-session -d -s "$SESSION"
    echo "[remote] Created tmux session '${SESSION}'"
fi

# Create window if absent, otherwise reuse
if ! tmux select-window -t "$TARGET" 2>/dev/null; then
    tmux new-window -t "$SESSION:" -n "$WINDOW"
    echo "[remote] Created tmux window '${WINDOW}'"
else
    echo "[remote] Reusing tmux window '${TARGET}'"
fi

# Clear any stale output and launch daemon
tmux send-keys -t "$TARGET" "" Enter
tmux send-keys -t "$TARGET" "cd ~/code/zeroclaw && ./target/release/zeroclaw daemon" Enter
echo "[remote] Daemon started in '${TARGET}'"
REMOTE

# ── 4. Health check ───────────────────────────────────────────────────────
log "Waiting for gateway ..."
for i in $(seq 1 10); do
    if ssh "$HOST" "curl -sf http://127.0.0.1:42617/health" &>/dev/null; then
        log "Gateway: OK"
        break
    fi
    if [[ "$i" -eq 10 ]]; then
        log "Gateway not responding after 10s — daemon may still be starting."
        log "Check: ssh ${HOST} -t 'tmux attach -t ${SESSION}'"
        exit 1
    fi
    sleep 1
done

# ── 5. Status ─────────────────────────────────────────────────────────────
log "Running zeroclaw status on ${HOST} ..."
ssh "$HOST" '$HOME/code/zeroclaw/target/release/zeroclaw status' 2>&1 || true

log ""
log "Migration complete."
log "  Attach: ssh ${HOST} -t 'tmux attach -t ${SESSION}'"
log "  Window: ${TMUX_TARGET}"
