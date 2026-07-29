# Aether Engine — Roadmap

> **Read this in other languages:** [Русский 🇷🇺](ROADMAP.ru.md)
>
> This roadmap reflects spec **v1.1 (Production Candidate)** and its **Progressive Enhancement** strategy.
> Status legend: ✅ done · 🚧 in progress · 📋 planned · ❄️ deferred · ❌ cancelled.

---

Aether follows a **phased delivery** model. Phases are sequenced by risk: the memory and
concurrency foundations must be proven before gameplay mechanics are layered on top.

### Phase 0 — Foundations & tooling
*Goal: a repository anyone can build, test and profile.*

- ✅ Cargo workspace skeleton (`aether-core`, `aether-world`, `aether-net`, `aether-convert`, `aether-telemetry`).
- ✅ Runtime SIMD dispatch scaffold (`Scalar / SSE4.2 / AVX2` real; `AVX-512` detected → AVX2 path until validated).
- ✅ CI: build + `cargo test` + `clippy` + `rustfmt` on x86-64.
- 🚧 Benchmark harness (Criterion): mask ops, storage round-trip, lighting, entity integrate, flow-field and redstone benches in place. Three of the four QA stress fixtures from the spec now exist — Redstone Stress (100k ≈ 2.9 ms), Entity Density (5k + AI ≈ 34 µs), World Edit/Explosion (500k edits); the Mass-Migration fixture still needs sample worlds.
- 🚧 Prometheus telemetry exporter wired; Tracy client stubbed behind a feature.

### Phase 1 — Core Engine MVP  *(Target: 70–75% Vanilla compliance)*
*Goal: a world that ticks stably at 20 TPS with the hard performance work done.*

- ✅ **Memory model**: `Sub-Chunk`, `AVX-Cell` (64-byte cache line), Morton (Z-order) indexing, SoA masks.
- 🚧 **Palette compression** (u4 / u8 → u16 auto-expand) ✅; Block-Entity arena still 📋.
- 🚧 **Physics engine**: basic voxel AABB collision + movement/gravity shipped (`aether-physics`); staged pipeline, SIMD broad-phase and the `Cached Environment` O(1) fast path still 📋.
- 🚧 **Redstone**: compiled Directed Dependency Graph landed (`aether-redstone`: source/wire/repeater/lamp in contiguous CSR memory, bounded worklist signal solve). Quasi-Connectivity, exact repeater/comparator delays and strict directional update order remain **accepted Phase 1 deviations** — see [Known Issues](KNOWN_ISSUES.md).
- 🚧 **Lighting**: flood-fill block + sky light landed (`aether-world::compute_light`, wired into the core API as `World::light_column`). Computes emitter block light and top-down sky light per chunk column with a BFS spread. The async safe-point scheduling and cross-chunk horizontal bleed are still 📋; `FullBright` remains the preview server's fallback.
- 🚧 **Entities/AI**: SoA ECS storage (`aether-entity`: generational `EntityId`, parallel component columns, batch integrate) ✅ and grid **Flow-Field navigation** (`aether-ai`: shared-goal integration field, O(1) per-mob steering) ✅. Cached single-agent A\* and the batched spawner are still 📋. A single `Player` entity also exists for the preview server.
- ✅ **Storage**: Fjall KV backend, Zstandard sub-chunk blobs, order-preserving keys (+ in-memory backend for tests).
- 📋 **World Engine** process model (one OS process per world).
- 📋 Work-stealing scheduler for generation / save / lighting off the main tick.

### Phase 2 — Hardcore Mechanical Target  *(Target: ≥ 95% Vanilla compliance)*
*Goal: close the deviation registry.*

- 📋 Full redstone dependency graph **with cross-chunk Quasi-Connectivity (QC)**.
- 📋 Deterministic fluid layering (1:1 with Vanilla tick timing).
- 📋 Strict directional-priority update order (replaces the parallel-graph ordering of Phase 1).
- 📋 Per-tick precise entity spawner (replaces batched spawner).
- 📋 Edge-case QA: Update Suppression micro-timings, historical engine quirks.

### Phase 3 — Network & multi-world
*Goal: real players, seamless transfers.*

- 📋 **Network Gateway** (Elixir/OTP or C++/Rust `epoll`/`io_uring`): TLS handshake, DDoS/rate-limit, packet sanitation.
- 📋 Internal Binary Protocol between Gateway and World Engines.
- 📋 Stateless seamless world hand-off (TCP stays open, route switches).
- 🚧 Vanilla client protocol compatibility layer. **Pulled forward** as an early preview: `aether-server` lets a **Minecraft 1.8.9 (protocol 47)** client connect and spawn in a full-bright flat world (offline, no compression/encryption). Its framing is verified against a raw socket client; a live-client pass and newer protocol versions are still ahead.

### Phase 4 — Extensibility, migration & ops
*Goal: production readiness.*

- 📋 **Plugin API**: Native C-ABI tier + WebAssembly (Wasmtime) sandbox + `engine.supports()` capability checks.
- 🚧 **Aether-Convert**: parallel Anvil `.mca` → KV migration with audit trail & checksums. **Pulled forward** — an initial version shipped alongside Phase 1 storage (both as the `aether-convert` crate and the standalone [Aether-Convert](https://github.com/lavrentijav/Aether-Convert) repo); Phase 4 hardens it (full block-state mapping, gzip/LZ4 chunks, resumable runs).
- 📋 In-game `/aether profile` visual sub-tick profiler.
- 📋 Ops hardening: graceful restart, backups, Grafana dashboards.

### Cross-cutting / continuous
- 🚧 Runnable demo (`aether-demo`) that exercises worldgen + physics + storage + telemetry end-to-end.
- 📋 Benchmark gates enforced in CI (regressions block merge).
- 📋 Documentation kept in sync with each shipped subsystem.
- ❄️ Non-x86 targets (ARM/NEON, RISC-V) — deferred until after x86-64 AVX2 baseline is stable.
