#!/usr/bin/env bash
# system-cleanup.sh — Reclaim disk space from build caches and package managers.
#
# Safe to run at any time. Only removes caches that tools rebuild on demand.
#
# Usage:
#   ./dev/system-cleanup.sh          # run all cleanups
#   ./dev/system-cleanup.sh --dry-run  # show what would be cleaned

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRY_RUN="${1:-}"

log() { echo "[cleanup] $*"; }

bytes_to_human() {
    local kb=$1
    if (( kb > 1048576 )); then
        echo "$(( kb / 1048576 ))G"
    elif (( kb > 1024 )); then
        echo "$(( kb / 1024 ))M"
    else
        echo "${kb}K"
    fi
}

before=$(df -k / | tail -1 | awk '{print $4}')
log "Disk free before: $(bytes_to_human "$before")"
echo

# ── 1. Zeroclaw cargo artifacts ────────────────────────────────────────────
log "=== Cargo build artifacts ==="
if [[ -x "$SCRIPT_DIR/cargo-clean-stale.sh" ]]; then
    bash "$SCRIPT_DIR/cargo-clean-stale.sh" "$DRY_RUN"
else
    log "cargo-clean-stale.sh not found, skipping"
fi
echo

# ── 2. Other Rust project targets ──────────────────────────────────────────
log "=== Other Rust project targets ==="
for proj in ~/code/*/Cargo.toml; do
    proj_dir="$(dirname "$proj")"
    proj_name="$(basename "$proj_dir")"
    target_dir="$proj_dir/target"
    # Skip zeroclaw (handled above) and projects with no target dir
    [[ "$proj_name" == "zeroclaw" ]] && continue
    [[ ! -d "$target_dir" ]] && continue
    size=$(du -sk "$target_dir" 2>/dev/null | awk '{print $1}')
    # Only clean if > 500MB
    if (( size > 512000 )); then
        if [[ "$DRY_RUN" == "--dry-run" ]]; then
            log "[dry-run] Would clean $proj_name/target ($(bytes_to_human "$size"))"
        else
            (cd "$proj_dir" && cargo clean 2>/dev/null)
            log "Cleaned $proj_name/target ($(bytes_to_human "$size"))"
        fi
    fi
done
echo

# ── 3. pip cache ───────────────────────────────────────────────────────────
log "=== pip cache ==="
pip_cache_dir="$HOME/Library/Caches/pip"
if [[ -d "$pip_cache_dir" ]]; then
    size=$(du -sk "$pip_cache_dir" 2>/dev/null | awk '{print $1}')
    if (( size > 102400 )); then
        if [[ "$DRY_RUN" == "--dry-run" ]]; then
            log "[dry-run] Would purge pip cache ($(bytes_to_human "$size"))"
        else
            python3 -m pip cache purge 2>/dev/null || true
            log "Purged pip cache ($(bytes_to_human "$size"))"
        fi
    else
        log "pip cache small ($(bytes_to_human "$size")), skipping"
    fi
else
    log "No pip cache found"
fi
echo

# ── 4. Homebrew cache ─────────────────────────────────────────────────────
log "=== Homebrew cache ==="
if command -v brew &>/dev/null; then
    brew_cache="$(brew --cache 2>/dev/null || echo "")"
    if [[ -n "$brew_cache" && -d "$brew_cache" ]]; then
        size=$(du -sk "$brew_cache" 2>/dev/null | awk '{print $1}')
        if (( size > 102400 )); then
            if [[ "$DRY_RUN" == "--dry-run" ]]; then
                log "[dry-run] Would clean Homebrew cache ($(bytes_to_human "$size"))"
            else
                brew cleanup --prune=0 2>/dev/null || true
                log "Cleaned Homebrew cache (was $(bytes_to_human "$size"))"
            fi
        else
            log "Homebrew cache small ($(bytes_to_human "$size")), skipping"
        fi
    fi
else
    log "Homebrew not installed"
fi
echo

# ── 5. uv cache ───────────────────────────────────────────────────────────
log "=== uv cache ==="
if command -v uv &>/dev/null; then
    uv_cache="$HOME/.local/share/uv"
    if [[ -d "$uv_cache" ]]; then
        size=$(du -sk "$uv_cache" 2>/dev/null | awk '{print $1}')
        if (( size > 102400 )); then
            if [[ "$DRY_RUN" == "--dry-run" ]]; then
                log "[dry-run] Would clean uv cache ($(bytes_to_human "$size"))"
            else
                uv cache clean 2>/dev/null || true
                log "Cleaned uv cache (was $(bytes_to_human "$size"))"
            fi
        else
            log "uv cache small ($(bytes_to_human "$size")), skipping"
        fi
    fi
else
    log "uv not installed"
fi
echo

# ── 6. pnpm cache ─────────────────────────────────────────────────────────
log "=== pnpm cache ==="
pnpm_cache="$HOME/Library/Caches/pnpm"
if [[ -d "$pnpm_cache" ]]; then
    size=$(du -sk "$pnpm_cache" 2>/dev/null | awk '{print $1}')
    if (( size > 102400 )); then
        if [[ "$DRY_RUN" == "--dry-run" ]]; then
            log "[dry-run] Would clean pnpm cache ($(bytes_to_human "$size"))"
        else
            pnpm store prune 2>/dev/null || rm -rf "$pnpm_cache"
            log "Cleaned pnpm cache (was $(bytes_to_human "$size"))"
        fi
    else
        log "pnpm cache small ($(bytes_to_human "$size")), skipping"
    fi
fi
echo

# ── 7. Go build cache ────────────────────────────────────────────────────
log "=== Go build cache ==="
go_cache="$HOME/Library/Caches/go-build"
if [[ -d "$go_cache" ]]; then
    size=$(du -sk "$go_cache" 2>/dev/null | awk '{print $1}')
    if (( size > 102400 )); then
        if [[ "$DRY_RUN" == "--dry-run" ]]; then
            log "[dry-run] Would clean Go build cache ($(bytes_to_human "$size"))"
        else
            go clean -cache 2>/dev/null || rm -rf "$go_cache"
            log "Cleaned Go build cache (was $(bytes_to_human "$size"))"
        fi
    else
        log "Go cache small ($(bytes_to_human "$size")), skipping"
    fi
fi
echo

# ── Summary ───────────────────────────────────────────────────────────────
after=$(df -k / | tail -1 | awk '{print $4}')
saved=$(( (after - before) / 1024 ))
log "Disk free after: $(bytes_to_human "$after")"
log "Reclaimed: ${saved}MB"
