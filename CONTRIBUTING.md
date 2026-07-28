# Contributing to Aether Engine

> **Read this in other languages:** [Русский 🇷🇺](CONTRIBUTING.ru.md)

---

Thanks for your interest in Aether! The project is in **early alpha**: the workspace
builds and tests, Phase 0 is done, and several Phase 1 subsystems have landed. The most
valuable contributions right now are the gameplay subsystems and hardening described in
the [Roadmap](docs/ROADMAP.md).

### Before you start
1. Read the [README](README.md), the [Roadmap](docs/ROADMAP.md), and the [Status](docs/STATUS.md).
2. Check [Known Issues](docs/KNOWN_ISSUES.md) — some "bugs" are **accepted Phase 1 deviations**, not defects.
3. Open (or comment on) a GitHub issue before large changes, so effort isn't duplicated.

### Development principles (non-negotiable)
These come straight from the spec and are what makes Aether *Aether*:
- **Data-Oriented Design.** Prefer Structure-of-Arrays over OOP object graphs.
- **Cache-locality first.** Keep data sequential; avoid pointer chasing.
- **SIMD-first.** Any hot math path needs a scalar fallback **and** vectorized SSE4.2/AVX2/AVX-512 branches selected via runtime dispatch.
- **No global locks.** Heavy work goes to the work-stealing pool and merges at deterministic Safe Points.
- **Determinism.** Parallel results must merge identically every run.

### Workflow
1. Fork and branch from the default branch: `git checkout -b feature/short-description`.
2. Keep changes focused; one logical change per pull request.
3. Run the local checks before pushing (see below).
4. Open a **draft** pull request early; describe *what* and *why*, link the issue.
5. Update the relevant docs (`docs/STATUS.md`, `docs/ROADMAP.md`) when a feature lands or a plan changes.

### Local checks
```bash
cargo fmt --all -- --check                        # formatting
cargo clippy --workspace --all-targets -- -D warnings   # lints (warnings are errors)
cargo test --workspace                            # correctness
cargo test --workspace --no-default-features      # no-default-features build
cargo bench -p aether-core                        # performance (no regressions vs. QA gates)
```
Performance-sensitive PRs should include a Tracy capture or the relevant benchmark result. The QA gates (redstone/entity/explosion/migration) live in the [README](README.md#benchmark-targets-qa-gates).

### Commit & PR style
- Clear, imperative commit subjects (e.g. `physics: add SIMD broad-phase mask check`).
- Reference issues (`Closes #123`).
- CI must be green; **do not** disable a failing check to merge.

### CI note for maintainers
After a build passes, there is **no need to re-poll the pull request on every run** — a
green build is sufficient signal. (See `CLAUDE.md`.)
