#!/usr/bin/env bash
# install-cron.sh — Install the daily cargo cleanup cron job on macOS.
#
# Merges with any existing crontab entries (won't duplicate if run twice).
#
# Usage:
#   ./dev/install-cron.sh           # install
#   ./dev/install-cron.sh --remove  # remove zeroclaw cron entries

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLEANUP_SCRIPT="$SCRIPT_DIR/cargo-clean-stale.sh"
SYSTEM_CLEANUP="$SCRIPT_DIR/system-cleanup.sh"
MARKER="# zeroclaw-auto-cleanup"

if [[ "${1:-}" == "--remove" ]]; then
    if crontab -l 2>/dev/null | grep -q "$MARKER"; then
        crontab -l 2>/dev/null | grep -v "$MARKER" | grep -v "cargo-clean-stale\|system-cleanup" | crontab -
        echo "Removed zeroclaw cron entries."
    else
        echo "No zeroclaw cron entries found."
    fi
    exit 0
fi

if ! [[ -x "$CLEANUP_SCRIPT" ]]; then
    echo "Error: $CLEANUP_SCRIPT not found or not executable" >&2
    exit 1
fi

# Build the new cron entries
NEW_ENTRIES=$(cat <<EOF
17 4 * * * $CLEANUP_SCRIPT >> /tmp/cargo-clean.log 2>&1 $MARKER
23 4 * * 0 $SYSTEM_CLEANUP >> /tmp/system-cleanup.log 2>&1 $MARKER
EOF
)

# Merge with existing crontab (remove old zeroclaw entries first to avoid dupes)
EXISTING=$(crontab -l 2>/dev/null | grep -v "$MARKER" | grep -v "cargo-clean-stale\|system-cleanup" || true)

if [[ -n "$EXISTING" ]]; then
    printf '%s\n%s\n' "$EXISTING" "$NEW_ENTRIES" | crontab -
else
    echo "$NEW_ENTRIES" | crontab -
fi

echo "Installed zeroclaw cron jobs:"
echo "  - Daily 4:17 AM: cargo-clean-stale.sh (prune build artifacts)"
echo "  - Weekly Sunday 4:23 AM: system-cleanup.sh (pip/brew/uv/pnpm/go caches)"
echo ""
echo "Current crontab:"
crontab -l
