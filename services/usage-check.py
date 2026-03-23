#!/usr/bin/env python3
"""
usage-check.py — Zero-token Claude Code quota checker.

Reads the OAuth token from macOS Keychain, hits the Anthropic usage endpoint,
and outputs JSON:
  {
    "exhausted": false,          # true if any bucket >= EXHAUSTED_THRESHOLD
    "five_hour_pct": 42.0,
    "seven_day_pct": 18.0,
    "reset_at": "2026-03-21T06:00:00Z",   # soonest reset for an exhausted bucket
    "error": null
  }

Exit code: 0 = ok to proceed, 1 = exhausted or error.

Usage:
  python3 services/usage-check.py
  python3 services/usage-check.py --threshold 90   # treat >=90% as exhausted
"""

import json
import subprocess
import sys
import urllib.request
import urllib.error

EXHAUSTED_THRESHOLD = 100.0  # percent
USAGE_URL = "https://api.anthropic.com/api/oauth/usage"
KEYCHAIN_SERVICE = "Claude Code-credentials"


def get_oauth_token() -> str:
    result = subprocess.run(
        ["security", "find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Keychain lookup failed: {result.stderr.strip()}")
    creds = json.loads(result.stdout.strip())
    return creds["claudeAiOauth"]["accessToken"]


def fetch_usage(token: str) -> dict:
    req = urllib.request.Request(
        USAGE_URL,
        headers={
            "Authorization": f"Bearer {token}",
            "anthropic-beta": "oauth-2025-04-20",
            "User-Agent": "claude-code/2.0.32",
        },
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())


def main():
    threshold = EXHAUSTED_THRESHOLD
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--threshold" and i + 1 < len(args):
            threshold = float(args[i + 1])
            i += 2
        else:
            i += 1

    try:
        token = get_oauth_token()
        data = fetch_usage(token)
    except Exception as e:
        out = {"exhausted": False, "error": str(e),
               "five_hour_pct": None, "seven_day_pct": None, "reset_at": None}
        print(json.dumps(out, indent=2))
        sys.exit(0)  # don't block triage on a lookup failure

    five_hour_pct = (
        data.get("five_hour", {}).get("utilization") or 0.0
    )
    seven_day_pct = (
        data.get("seven_day", {}).get("utilization") or 0.0
    )

    exhausted = False
    reset_at = None

    for bucket_key in ("five_hour", "seven_day", "seven_day_sonnet", "seven_day_opus"):
        bucket = data.get(bucket_key) or {}
        pct = bucket.get("utilization") or 0.0
        if pct >= threshold:
            exhausted = True
            if reset_at is None:
                reset_at = bucket.get("resets_at")

    out = {
        "exhausted": exhausted,
        "five_hour_pct": five_hour_pct,
        "seven_day_pct": seven_day_pct,
        "reset_at": reset_at,
        "error": None,
    }
    print(json.dumps(out, indent=2))
    sys.exit(1 if exhausted else 0)


if __name__ == "__main__":
    main()
