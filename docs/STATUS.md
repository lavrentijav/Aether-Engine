# Aether Engine — Project Status

> **Read this in other languages:** [Русский 🇷🇺](STATUS.ru.md)
>
> Single source of truth for **what exists**, **what is planned**, and **what is cancelled**.
>
> Legend: ✅ done · 🚧 in progress · 📋 planned · ❄️ deferred · ❌ cancelled.
>
> Last reviewed: 2026-07-29 · Spec: v1.1

---

### ✅ What exists today
The project has left pure design: **Phase 0 is implemented**, several Phase 1
subsystems have landed, and there is now a runnable demo plus an experimental
server a vanilla 1.8.9 client can connect to.

| Item | State |
|------|-------|
| Technical specification (v1.0 & v1.1) | ✅ Written (source PDFs) |
| README (English + Russian) | ✅ |
| Roadmap, Status, Known Issues, Contributing docs | ✅ |
| License | ✅ |
| `.gitignore` for a Cargo project | ✅ |
| Cargo workspace + crate skeletons | ✅ |
| Runtime SIMD dispatch (Scalar/SSE4.2/AVX2 real, AVX-512 detected) + Morton | ✅ |
| Telemetry: counters/gauges/timers + Prometheus exporter | ✅ |
| SoA memory model: AVX-Cell, Sub-Chunk, Morton masks, palette compression | ✅ |
| KV world storage (Fjall + Zstandard blobs, order-preserving keys) | ✅ |
| `aether-convert`: Anvil `.mca` → KV migration (parallel, audited) | ✅ |
| `aether-baseproxy`: live vanilla → core translation (network chunk sections, block states) | ✅ |
| Basic physics: voxel AABB collision, movement + gravity (`aether-physics`) | ✅ |
| Chunk generation: flat + value-noise terrain (`aether-worldgen`) | ✅ |
| Core API: `World` facade over storage/generation/physics (`aether-api`) | ✅ |
| Runnable demo: `aether` binary (worldgen + physics + storage + telemetry) with TOML config | ✅ |
| Player entity | ✅ |
| Lighting: flood-fill block + sky light (`compute_light` / `World::light_column`); per-column, cross-chunk bleed still deferred. `FullBright` fallback kept for the preview server | ✅ |
| SoA entity storage (`aether-entity`): generational `EntityId`, parallel component columns, batch integrate | ✅ |
| Flow-Field crowd navigation (`aether-ai`): shared-goal integration field + O(1) per-mob steering; cached A* still planned | ✅ |
| Graph-based redstone (`aether-redstone`): compiled CSR Directed Dependency Graph + worklist signal solve; QC / exact delays / strict order deferred to Phase 2 | ✅ |
| Experimental join server `aether-server` (1.8.9 / protocol 47, superflat, creative, full-bright) — protocol verified against a raw socket client, **not yet against a live client** | 🚧 Preview |
| CI (build/test/clippy/fmt) + Criterion bench harness | ✅ |
| Higher-level mob AI (behaviours, spawner) / full staged physics pipeline | ❌ Not yet started |

### 📋 What is planned
Grouped by roadmap phase (see [ROADMAP.md](ROADMAP.md) for the full sequence).

**Phase 0 — Foundations**
- ✅ Cargo workspace + crate skeletons
- ✅ Runtime SIMD dispatch trait & backends
- ✅ CI (build, test, clippy, fmt) + Criterion benches
- 🚧 Telemetry hooks (Prometheus exporter ✅; Tracy client stubbed behind a feature)

**Phase 1 — Core Engine MVP (70–75% Vanilla)**
- ✅ SoA memory model: Sub-Chunk, AVX-Cell, Morton order, masks
- ✅ Palette compression (u4/u8/u16 auto-expand) — Block-Entity arena still 📋
- 🚧 Physics pipeline (basic collision + gravity ✅; SIMD broad-phase, cached environment 📋)
- 🚧 Graph-based redstone (DDG): compiled CSR graph + signal solve ✅ (`aether-redstone`); QC, exact delays, strict update order 📋 (Phase 2 deviations)
- 🚧 Lighting (cell flood-fill): block + sky light ✅ (per-column); async safe-point merge + cross-chunk bleed 📋. `FullBright` fallback kept for the preview server
- 🚧 ECS entities: SoA storage ✅ (`aether-entity`) + Flow-Field navigation ✅ (`aether-ai`); cached A* + batched spawner 📋
- ✅ KV storage (Fjall + Zstd)
- 📋 Per-world process isolation + work-stealing scheduler

**Phase 2 — Hardcore Mechanical Target (≥ 95% Vanilla)**
- 📋 Cross-chunk Quasi-Connectivity redstone
- 📋 Deterministic fluids, strict update order, per-tick spawner

**Phase 3 — Network & multi-world**
- 📋 Network Gateway, Internal Binary Protocol, seamless hand-off, Vanilla protocol
- 🚧 Experimental 1.8.9 join server (`aether-server`) as an early preview of the Vanilla-protocol layer

**Phase 4 — Extensibility & ops**
- 📋 Native C-ABI + WASM plugin API
- 🚧 Aether-Convert migration tool (initial version shipped; hardening ahead)
- 📋 In-game profiler, ops hardening

### ❌ What is cancelled / superseded
Decisions explicitly dropped or replaced during design. Kept here so the history is not lost.

| Cancelled / changed | Reason | Replaced by |
|---------------------|--------|-------------|
| **"100% Vanilla-accurate from day one"** (spec v1.0: *"priority is given to perfect Vanilla mechanics"*) | Blocked MVP on exotic edge-cases | **Progressive Enhancement** (v1.1): 70–75% in Phase 1, 95%+ in Phase 2 |
| **Per-tick precise entity spawner in Phase 1** | Too costly for the MVP tick budget | Batched async spawner in Phase 1; per-tick spawner deferred to Phase 2 |
| **Exact QC (Quasi-Connectivity) across chunk borders in Phase 1** | Requires the full cross-chunk dependency graph | Ignored in Phase 1 (registered deviation); implemented in Phase 2 |
| **OOP entity model** (`struct Player { ... }`) | Poor cache locality, blocks SIMD | Structure-of-Arrays (SoA) storage |
| **Fork of an existing Java server** (Paper/Purpur/Folia/Fabric) | Locks in Java internals; defeats the DOD/SIMD goal | Clean-room Rust engine |
| ❄️ **Non-x86 (ARM/NEON, RISC-V) targets** | Not cancelled — **deferred** until x86-64 AVX2 baseline is stable | Revisit post-Phase 1 |
