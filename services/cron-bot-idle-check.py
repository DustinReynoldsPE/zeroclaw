#!/usr/bin/env python3
"""
cron-bot-idle-check.py — Deterministic Matrix room idle state checker.

Outputs JSON:
  {
    "idle": true,                         # last human msg > IDLE_THRESHOLD_HOURS ago
    "last_sender": "@user:example.com",
    "last_ts_ms": 1710000000000,
    "last_ts_human": "2026-03-20T21:00:00Z",
    "cron_bot_posted_recently": false,    # cron-bot posted within DEDUP_HOURS
    "cron_bot_last_ts_ms": null,
    "should_post": true                   # idle AND cron_bot not recent
  }

Usage:
  python3 services/cron-bot-idle-check.py <room_id>
  python3 services/cron-bot-idle-check.py <room_id> --config ~/.zeroclaw/cron-bot.json

Exit code: 0 if should_post, 1 otherwise (including errors).
"""

import json
import sys
import os
import time
import urllib.parse
import urllib.request
import urllib.error
from datetime import datetime, timezone

IDLE_THRESHOLD_HOURS = 4
DEDUP_HOURS = 3
HISTORY_LIMIT = 50


def load_config(config_path: str) -> dict:
    with open(config_path) as f:
        return json.load(f)


def matrix_get(homeserver: str, token: str, path: str) -> dict:
    url = homeserver.rstrip("/") + path
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())


def ts_to_human(ts_ms):
    if ts_ms is None:
        return None
    return datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def check_room(homeserver: str, token: str, cron_bot_user_id: str, room_id: str) -> dict:
    now_ms = int(time.time() * 1000)
    idle_threshold_ms = IDLE_THRESHOLD_HOURS * 3600 * 1000
    dedup_threshold_ms = DEDUP_HOURS * 3600 * 1000

    encoded_room = urllib.parse.quote(room_id, safe="")
    path = f"/_matrix/client/v3/rooms/{encoded_room}/messages?dir=b&limit={HISTORY_LIMIT}"

    try:
        data = matrix_get(homeserver, token, path)
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}", "should_post": False}
    except Exception as e:
        return {"error": str(e), "should_post": False}

    events = data.get("chunk", [])
    messages = [
        e for e in events
        if e.get("type") == "m.room.message"
        and isinstance(e.get("content", {}).get("body"), str)
    ]

    last_human_ts_ms = None
    last_human_sender = None
    cron_bot_last_ts_ms = None

    for e in messages:
        sender = e.get("sender", "")
        ts = e.get("origin_server_ts", 0)
        if sender == cron_bot_user_id:
            if cron_bot_last_ts_ms is None or ts > cron_bot_last_ts_ms:
                cron_bot_last_ts_ms = ts
        else:
            if last_human_ts_ms is None or ts > last_human_ts_ms:
                last_human_ts_ms = ts
                last_human_sender = sender

    idle = last_human_ts_ms is None or (now_ms - last_human_ts_ms) >= idle_threshold_ms
    cron_bot_posted_recently = (
        cron_bot_last_ts_ms is not None
        and (now_ms - cron_bot_last_ts_ms) < dedup_threshold_ms
    )

    return {
        "idle": idle,
        "last_sender": last_human_sender,
        "last_ts_ms": last_human_ts_ms,
        "last_ts_human": ts_to_human(last_human_ts_ms),
        "cron_bot_posted_recently": cron_bot_posted_recently,
        "cron_bot_last_ts_ms": cron_bot_last_ts_ms,
        "cron_bot_last_ts_human": ts_to_human(cron_bot_last_ts_ms),
        "should_post": idle and not cron_bot_posted_recently,
    }


def main():
    args = sys.argv[1:]
    if not args or args[0] in ("-h", "--help"):
        print(__doc__)
        sys.exit(0)

    room_id = args[0]
    config_path = None
    i = 1
    while i < len(args):
        if args[i] == "--config" and i + 1 < len(args):
            config_path = args[i + 1]
            i += 2
        else:
            i += 1

    if config_path is None:
        config_path = os.path.expanduser("~/.zeroclaw/cron-bot.json")

    cfg = load_config(config_path)
    result = check_room(cfg["homeserver"], cfg["access_token"], cfg["user_id"], room_id)
    print(json.dumps(result, indent=2))
    sys.exit(0 if result.get("should_post", False) else 1)


if __name__ == "__main__":
    main()
