#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SERVICE_TEMPLATE="$SCRIPT_DIR/zeroclaw.service"
SERVICE_NAME="zeroclaw"
TARGET_USER="${1:-$(whoami)}"
TARGET_HOME="$(eval echo "~$TARGET_USER")"

if [[ $EUID -ne 0 ]]; then
    echo "Run with sudo: sudo $0 [username]"
    exit 1
fi

if [[ ! -f "$TARGET_HOME/code/zeroclaw/target/release/zeroclaw" ]]; then
    echo "Binary not found at $TARGET_HOME/code/zeroclaw/target/release/zeroclaw"
    echo "Build first: cargo build --release"
    exit 1
fi

echo "Installing $SERVICE_NAME service for user $TARGET_USER ($TARGET_HOME)"

sed -e "s|ZEROCLAW_USER|$TARGET_USER|g" \
    -e "s|ZEROCLAW_HOME|$TARGET_HOME|g" \
    "$SERVICE_TEMPLATE" > "/etc/systemd/system/$SERVICE_NAME.service"

systemctl daemon-reload
systemctl enable "$SERVICE_NAME"
systemctl start "$SERVICE_NAME"

sleep 1
systemctl status "$SERVICE_NAME" --no-pager
