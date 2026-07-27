# CLAUDE.md — Project context & working rules

Project context and conventions for AI agents (Claude Code) working in this repository.

## About the project
**Aether Engine** is a from-scratch, high-performance Minecraft server engine written in
Rust (see [`README.md`](README.md)). It is **not** a fork of any Java server. Design
pillars: Data-Oriented Design (SoA), SIMD-first (Scalar/SSE4.2/AVX2/AVX-512 via runtime
dispatch), cache-locality first (AVX-Cell = 64-byte cache line, Morton order), lock-free /
work-stealing parallelism, and deterministic Safe-Point merges.

Authoritative docs:
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — phased plan (Phase 0 → 4)
- [`docs/STATUS.md`](docs/STATUS.md) — what exists / planned / cancelled
- [`docs/KNOWN_ISSUES.md`](docs/KNOWN_ISSUES.md) — problems, risks, accepted deviations
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — build/test/PR workflow

## Working rules

### CI / pull-request watching
- **When GitHub Actions are added to this repo, do NOT re-check / re-poll the pull request
  on every run once a build has succeeded.** A green build is a sufficient signal — stop
  re-inspecting the PR after a successful build instead of checking it each time.

### Docs & compatibility
- Keep `docs/STATUS.md` and `docs/ROADMAP.md` in sync when a feature lands or a plan changes.
- Before treating a compatibility gap as a bug, check the Known Deviations Registry in
  `docs/KNOWN_ISSUES.md` — Phase 1 deviations are intentional, not defects.
