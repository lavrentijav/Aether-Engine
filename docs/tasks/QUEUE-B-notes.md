# Queue B notes

## Workspace-wide clippy is blocked by crates this queue does not touch

`cargo clippy --workspace --all-targets -j4 -- -D warnings` does not build on
this worktree: `aether-server` fails with `ItemTypeId: From<&str>` errors in
`crates/aether-server/src/game/provenance.rs`.

Cause: `oc/QB` branched from `ca441aa`, and `main` has since advanced 18
commits (including `oc/QE`'s 1.17 chunk work and the `ItemTypeId` change) that
the branch does not carry. COMMON forbids merging `main` into the queue
branch, so the stale `aether-server` cannot be fixed from here.

What was run instead, per task, all passing:

- `cargo fmt --all -- --check`
- `cargo clippy -p aether-worldgen --all-targets -j4 -- -D warnings`
- `cargo test -p aether-worldgen -j4`

The crates touched by this queue (`aether-worldgen`, and its dependency
`aether-world`) are clean. `cargo check -p aether-server` on `main` passes, so
the failure is purely the branch's age, not the worldgen changes.

## Parity example

`examples/vanilla_world_compare.rs` needs a vanilla-generated region plus its
seed (`<pack-root> <seed> <region-dir>`), same as `tests/vanilla_parity.rs`.
No such reference world is present on this machine (no `AETHER_VANILLA_REGION`
/ `AETHER_VANILLA_SEED`, no `.mca` under the build tree), so it was not run
and no before/after numbers are reported. It reads only the pack and the
region files—not the game jar—so it can be run once a reference world is
supplied.
