# T01 — Repository hygiene: cargo-deny config, unused deps, pinned toolchain

## Context
Aether Engine is a Rust workspace (`/srv/dev/projects/Aether-Engine`, crates in `crates/`).
CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings`, build, tests. A review found three
small hygiene gaps. Fix exactly these; change nothing else.

## Where to work
Your own git worktree and branch — never edit the shared checkout:
```
git -C /srv/dev/projects/Aether-Engine worktree add /srv/dev/projects/Aether-Engine-wt/T01 -b oc/T01
cd /srv/dev/projects/Aether-Engine-wt/T01
```
Build inside the worktree (its own `target/`). Use at most `-j4`.

## Tasks
1. **`deny.toml` at the workspace root** so `cargo deny check` passes with no errors.
   - `[licenses]`: allow the permissive licenses actually used by the dependency tree
     (check with `cargo deny list` or the failure output): at least `MIT`, `Apache-2.0`,
     `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-3.0`,
     `Zlib`, `0BSD` — only those that appear. Workspace crates are `MIT OR Apache-2.0`.
   - `[advisories]`: default (deny vulnerabilities, warn unmaintained).
   - `[bans]`: `multiple-versions = "warn"`, `wildcards = "deny"` (path deps of the workspace
     must not trigger it — use `allow-wildcard-paths = true`).
   - `[sources]`: only crates.io allowed.
   - Add a step `cargo deny check` to `.github/workflows/ci.yml` using
     `EmbarkStudios/cargo-deny-action@v2` after the Formatting step.
2. **Remove unused dependencies** reported by `cargo machete`:
   - `aether-core` from `crates/aether-worldgen/Cargo.toml`
   - `aether-physics` from `crates/aether-demo/Cargo.toml`
   Verify first with `grep -rn "aether_core" crates/aether-worldgen` and
   `grep -rn "aether_physics" crates/aether-demo` (including tests/benches/examples). If one IS used
   by tests/examples, move it to `[dev-dependencies]` instead of deleting.
3. **Pin the toolchain**: add `rust-toolchain.toml`:
   ```toml
   [toolchain]
   channel = "1.99.0"
   components = ["rustfmt", "clippy"]
   ```
   and in `ci.yml` keep `dtolnay/rust-toolchain` but set `toolchain: 1.99.0`. Do not change
   `rust-version` (MSRV) in `Cargo.toml`.

Tools: `cargo-deny`, `cargo-machete` are in `/srv/dev/tools/bin` (add it to PATH if needed).

## Done when (run all, in the worktree)
- `cargo deny check` → no errors
- `cargo machete` → no unused dependencies
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -j4 -- -D warnings`
- `cargo test --workspace -j4`

Commit on `oc/T01` with a meaningful message (e.g. `chore: deny.toml, drop unused deps, pin toolchain 1.99.0`).
Do not push, do not merge, do not touch other branches, no sudo, no edits outside the worktree.

## Report back
Branch, commit hash, output summary of each check, anything you could not do.
