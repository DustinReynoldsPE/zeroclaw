---
id: increase-release-codegen-18b0
stage: done
deps: []
links: []
created: 2026-03-24T22:17:24Z
type: task
priority: 2
assignee: Dustin Reynolds
tags: [rust, build]
version: 5
---
# Increase release codegen-units to speed up builds

Release builds are bottlenecked on single-threaded LLVM codegen for the final crate. hpllm has 4 cores but only 1 is used during the zeroclawlabs codegen phase because profile.release defaults to codegen-units=1. Setting codegen-units=4 in Cargo.toml would trade a small optimization loss for ~2-3x faster final compilation. Evaluate the binary size and performance impact before merging.
