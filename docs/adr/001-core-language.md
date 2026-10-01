# ADR 001 — Core language: Rust

**Status:** accepted (milestone 01)
**Date:** 2026-10-01

## Context

SeaLion needs a systems language for a from-scratch search engine: inverted
indexes with tight memory layout control, compressed postings decoders on the
hot path, concurrent indexing/querying/merging, and a networked distributed
layer. The spec (§3) prefers Rust, allowing C++20+ only with a compelling
profiling-backed reason.

## Alternatives considered

1. **Rust (stable)** — memory safety without GC, fearless concurrency, strong
   ecosystem for the allowed infra (tokio, axum/hyper, serde), excellent
   testing/property/fuzz tooling (proptest, cargo-fuzz), first-class
   benchmarking (criterion).
2. **C++20+** — maximum low-level control and mature SIMD idioms; weaker
   safety story, slower iteration, harder to get correct concurrent code right
   the first time. The spec demands correctness-first engineering (§1), which
   favors the borrow checker.
3. **Go** — great concurrency ergonomics, but GC pauses complicate p99
   latency claims, and cache-level control over postings layout is weaker.
4. **Python/Java** — ruled out for the engine hot path (throughput, memory
   layout, latency control). Python remains the choice for offline evaluation
   tooling (§3).

## Decision

**Rust (stable) for the entire engine**: index, query, crawler, distributed
layer, API, CLI, and benchmark harnesses. React/TypeScript for the web UI and
ops console (milestones 19–20); Python for offline evaluation tooling
(milestone 12).

## Tradeoffs

- **Upfront cost:** fighting the borrow checker on self-referential index
  structures (e.g., segment readers borrowing mmap'd buffers). Mitigated with
  arena/offset-based designs and `Arc` where sharing is genuine.
- **Unsafe:** postings decoders and SIMD paths may eventually need small
  `unsafe` blocks; each must be justified, documented, and fuzz-tested
  (§98–99).
- **Compile times:** workspace split into focused crates (§107) keeps
  incremental builds fast; release LTO is enabled for benchmarks.

## Consequences

- All IR structures and algorithms are hand-implemented in safe Rust by
  default (§2 core restriction).
- CI enforces `cargo fmt --check`, `clippy -D warnings`, and the full test
  suite per PR (§109).
- Revisit only if profiling shows a compelling, measured reason (§116).
