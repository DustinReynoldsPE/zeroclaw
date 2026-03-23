---
id: tiered-model-routing-63c1
stage: done
deps: []
links: []
created: 2026-03-19T15:29:17Z
type: feature
priority: 2
assignee: Dustin Reynolds
tags: [providers, cost, routing]
skipped: [verify]
version: 7
---
# Tiered model routing — use cheaper models for routine tasks

Currently all rooms default to Opus via claude-code. Introduce tiered model routing so cheaper models (Sonnet, Haiku) handle routine tasks like simple Q&A, status checks, and memory lookups, while Opus is reserved for tmux interactive sessions and complex multi-tool workflows. Options: (1) route by message complexity — short/simple messages go to a lighter model, (2) per-room model tiers in channel_providers config, (3) automatic escalation — start with Sonnet and escalate to Opus when tool calls or reasoning depth is needed. This would significantly reduce token costs for high-volume rooms like General and LocalLLM while preserving full capability in tmux-routed sessions.

## Notes

**2026-03-19T15:48:39Z**

Implemented via config: query_classification routes short simple messages (<=150 chars, common keywords) to claude-code --model sonnet, complex messages (>=50 chars, engineering keywords) to claude-code default (Opus). Tmux-routed sessions bypass classification. No code changes needed — existing ModelRouteConfig + QueryClassificationConfig infrastructure used.
