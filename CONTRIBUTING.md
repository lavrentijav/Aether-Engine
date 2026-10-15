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

### Core or plugin?
The engine core is for what every server needs and what must be fast: world model, storage and
journal, worldgen, physics, the tick, vanilla mechanics, redstone, light, the protocol and the compat
layer. **Features that are optional for a server are not written in the core**; they are written as
plugins on the plugin API ([`docs/design/plugins.md`](docs/design/plugins.md)). There are two plugin
tiers:

- **Native plugin** — a Rust crate under `plugins/<name>/` in this workspace, linked **in-process**
  on the same plugin API/events as a WASM plugin but with **no boundary and full speed**. Use it for
  optional features that are **performance-sensitive or deeply integrated** with engine data:
  permissions store + `/lp`, regions/claims model and checks, WorldEdit, history commands
  (`/co`, `/restore`), the profiler, moderation, essentials-teleport/player/inv.
- **WASM plugin** — a sandboxed component (Wasmtime), untrusted third-party style, for optional and
  **not** performance-critical features: economy, shops, chat formatting, social, fun, integrations,
  stats.

Rules of thumb:
- *Would a vanilla-like server run without it?* Yes → plugin (native or WASM by the next rule).
- *Is it optional but performance-sensitive or deeply integrated?* → **native plugin**.
- *Is it optional, not performance-critical, and safe to sandbox?* → **WASM plugin**.
- *Is it on the per-tick hot path (runs for every block/entity/packet) or needed by every server?*
  → core, **with no policy**: the core runs a fixed hook and the decision lives in a plugin. A
  mechanism an operator could want to change is policy, and policy is never core.
- *Does it need data the plugin API does not expose?* Extend the API (a new event, registry or host
  import, versioned and behind `supports()`), do not move the feature into the core.
- The core must build and run with **every** plugin disabled. Built-ins that already exist and are
  optional (economy, `/stash`, rewards) migrate to plugins; the core keeps only the service
  interface they implement.

### Workflow
1. Fork and branch from the default branch: `git checkout -b feature/short-description`.
2. Keep changes focused; one logical change per pull request.
3. Run the local checks before pushing (see below).
4. Open a **draft** pull request early; describe *what* and *why*, link the issue.
5. Update the relevant docs (`docs/STATUS.md`, `docs/ROADMAP.md`) when a feature lands or a plan changes.

### Local checks
The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml) (Rust
**1.99.0** with `rustfmt` and `clippy`), so `rustup` uses the right version and
components automatically.

```bash
cargo fmt --all -- --check                        # formatting
cargo clippy --workspace --all-targets -- -D warnings   # lints (warnings are errors)
cargo test --workspace                            # correctness
cargo test --workspace --no-default-features      # no-default-features build
cargo deny check                                  # licenses, advisories, bans, sources
cargo bench -p aether-core                        # performance (no regressions vs. QA gates)
```
`cargo deny check` reads [`deny.toml`](deny.toml); CI runs it on every push and
PR via `cargo-deny-action`. Install it with `cargo install cargo-deny` if the
command is missing locally.

Performance-sensitive PRs should include a Tracy capture or the relevant benchmark result. The QA gates (redstone/entity/explosion/migration) live in the [README](README.md#benchmark-targets-qa-gates).

### Commit & PR style
- Clear, imperative commit subjects (e.g. `physics: add SIMD broad-phase mask check`).
- Reference issues (`Closes #123`).
- CI must be green; **do not** disable a failing check to merge.

### CI note for maintainers
After a build passes, there is **no need to re-poll the pull request on every run** — a
green build is sufficient signal. (See `CLAUDE.md`.)
