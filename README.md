# Aether-Engine

> **Read this in other languages:** [Русский 🇷🇺](README.ru.md)

**A next-generation, high-performance Minecraft server engine built from scratch in Rust.**

Project codename: **Aether** · Spec version: **1.1 (Production Candidate)** · Status: **Pre-alpha / design phase**

---

## What is Aether?

Aether Engine is a clean-room game-server engine for Minecraft, written from scratch in Rust. It is **not** a fork or a patch of the Java server family (Paper, Purpur, Folia, Fabric). Instead it re-imagines the server internals around modern hardware:

- **Data-Oriented Design (DOD)** with Structure-of-Arrays (SoA) storage.
- **SIMD-first** math (Scalar → SSE4.2 → AVX2 → AVX-512, selected at runtime).
- **Cache-locality first** memory layout with Morton (Z-order) indexing.
- **Lock-free / work-stealing** parallelism instead of global locks.

The goal: serve **1000+ players in a single game space** at a stable **20 TPS (50 ms/tick)**, with physics, redstone, lighting and AI computed in parallel.

## Core design goals

| Goal | Approach |
|------|----------|
| Maximum CPU utilization | Vectorized (SIMD) subsystems + work-stealing scheduler |
| Network / logic isolation | Stateless **Network Gateway** ⇄ isolated per-world **World Engine** processes |
| Real-time observability | Built-in Tracy + Prometheus sub-tick telemetry |
| Safe extensibility | Two-tier plugins: **Native C-ABI** (fast) + **WebAssembly** (sandboxed) |
| Legacy world support | `Aether-Convert` CLI: Anvil `.mca` → KV store |
| Predictable compatibility | **Progressive Enhancement** (70–75% Vanilla in Phase 1 → 95%+ in Phase 2) |

## Architecture at a glance

```
            [ Client traffic (Internet) ]
                        │
                        ▼
            ┌──────────────────────┐
            │   Network Gateway    │   Stateless: TLS, DDoS/rate-limit,
            │   (Elixir/OTP / C++) │   packet sanitation, routing
            └───────────┬──────────┘
                        │  Internal Binary Protocol
        ┌───────────────┼───────────────┐
        ▼               ▼               ▼
 ┌────────────┐  ┌────────────┐  ┌────────────┐
 │ World #1   │  │ World #2   │  │ World #N   │   Each world = isolated
 │ (Rust)     │  │ (Rust)     │  │ (Rust)     │   OS process. A crash in
 └────────────┘  └────────────┘  └────────────┘   one world never touches
                                                   the others or the Gateway.
```

### Memory hierarchy

```
World → Region → Chunk (16×16) → Sub-Chunk (16×16×16) → AVX-Cell (4×4×2)
```

The **AVX-Cell** (32 blocks × `u16` = **64 bytes**) is the atomic unit of memory — exactly one CPU cache line. It is read in **1** AVX-512 op, **2** AVX2 ops, or **4** SSE ops. Sub-chunk data is split into SoA masks (`SolidMask`, `LightOpacity`, `Collision`, `RedstoneFlags`, `BlockStateID`) so each subsystem touches only the bytes it needs.

## Key subsystems

- **Physics** — staged pipeline (`Input → Movement → Collision → Environment → Velocity → Finalize`) with SIMD broad-phase and an O(1) cached-environment fast path.
- **Redstone** — a compiled **Directed Dependency Graph (DDG)**: zero spatial queries when nothing changes.
- **Lighting** — asynchronous cell-based flood-fill with safe-point merges.
- **Entities / AI** — ECS storage + Flow-Field navigation (O(1) pathing for crowds) with cached A\* for singles.
- **Storage** — KV database (RocksDB / Fjall), Zstandard-compressed sub-chunk blobs.
- **Telemetry** — Tracy zones per tick phase + Prometheus exporter + in-game `/aether profile`.

## Documentation

| Document | Description |
|----------|-------------|
| [Roadmap](docs/ROADMAP.md) | Phased delivery plan (Phase 0 → Phase 4) |
| [Status](docs/STATUS.md) | What exists · what's planned · what's cancelled |
| [Known Issues](docs/KNOWN_ISSUES.md) | Current problems, risks & accepted deviations |
| [Contributing](CONTRIBUTING.md) | How to build, test and submit changes |

## Project status

Aether is currently in the **design / pre-alpha** phase. The repository holds the
specification and planning documents; the engine crates are not yet implemented.
See [`docs/STATUS.md`](docs/STATUS.md) for the authoritative feature checklist.

## Benchmark targets (QA gates)

| Test | Requirement |
|------|-------------|
| Redstone stress (100 000 active components) | ≤ 5 ms processing |
| Entity density (5 000 AI entities, one area) | 20.0 TPS |
| Explosion (500 000 blocks at once) | No TPS drop |
| Mass migration (50 GB world) | ≥ 5 000 chunks/sec, 0 fatal errors |

## License

See [LICENSE](LICENSE).
