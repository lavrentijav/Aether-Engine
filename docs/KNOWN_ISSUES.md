# Aether Engine — Known Issues & Deviations

> **Read this in other languages:** [Русский 🇷🇺](KNOWN_ISSUES.ru.md)
>
> Current problems, engineering risks, and the **Known Deviations Registry** (Phase 1
> compatibility gaps that are accepted on purpose).
>
> Last reviewed: 2026-07-29

---

### A. Current known problems (early alpha)
1. **Gameplay subsystems unbuilt.** Storage, basic physics, worldgen, the core API and a flood-fill lighting engine (block + sky light per chunk column) exist, but redstone, ECS entities/AI and the full staged physics pipeline are not implemented yet. Lighting is computed synchronously per column — the async safe-point scheduler and cross-chunk horizontal bleed are still ahead, and the preview server still uses the `FullBright` fallback.
2. **SIMD parity only partially validated.** `Scalar / SSE4.2 / AVX2` paths are implemented and unit-tested; `AVX-512` is *detected* but routed to the AVX2 path — there is no native AVX-512 backend yet.
3. **Determinism unproven.** Parallel subsystems must merge at Safe Points; the merge points are specified but not validated, and the work-stealing scheduler that would exercise them does not exist yet.
4. **Experimental 1.8.9 server is unverified against a live client.** `aether-server` speaks protocol 47 in offline mode with no compression/encryption, and its framing is checked only against a raw socket client. A real Minecraft 1.8.9 client may still reject some packets (chunk-data format is the most likely gap). It binds to loopback by default and must not be exposed publicly.

### B. Known Deviations Registry (Phase 1 — accepted on purpose)
These are **not bugs** in Phase 1 — they are documented, temporary compatibility gaps that must be recorded in the engine config and closed in Phase 2.

| Subsystem | Phase 1 deviation | Phase 2 target |
|-----------|-------------------|----------------|
| **Redstone** | Quasi-Connectivity (QC) ignored across inactive chunk borders | Full dependency graph with cross-chunk QC |
| **Fluids** | Parallel simplified spread; Java tick timing not preserved | Deterministic fluid layers 1:1 with Vanilla |
| **Update order** | Simultaneous redstone updates ordered by the parallel graph | Strict deterministic directional-priority queue |
| **Entity spawn** | Batched async spawn every N ticks | Per-tick precise spawner |
| **Lighting** | Flood-fill computed **per chunk column**: no horizontal light bleed across chunk borders, and the spread is synchronous (no async safe-point merge). Preview server still ships `FullBright` | Async cell-based flood-fill with safe-point merges and cross-chunk bleed |

### C. Engineering risks & trade-offs
- **RAM overhead: +15–25%.** SoA masks, Morton order, and redstone graphs cost more memory than classic structures. Accepted for cache-locality and SIMD wins.
- **x86-64 first.** The first version targets x86-64 with AVX2; other ISAs are deferred. Users on ARM/RISC-V are unsupported until later.
- **Determinism vs. parallelism.** Aggressive parallelism increases the risk of non-deterministic results if Safe-Point merges are wrong — needs strong test coverage.
- **Compatibility ceiling in Phase 1.** 70–75% Vanilla compliance means some contraptions (QC-dependent redstone, tick-perfect fluids) will behave differently until Phase 2.
- **External dependency choices.** RocksDB vs. Fjall, Elixir/OTP vs. C++/Rust for the Gateway — not finalized; a wrong pick is expensive to reverse.

### D. How to report a problem
Open a GitHub issue with: the subsystem, whether it's a **bug** or an **accepted deviation** (check the table above first), reproduction steps, and — for performance issues — a Tracy capture or the relevant benchmark scenario. See [CONTRIBUTING.md](../CONTRIBUTING.md).
