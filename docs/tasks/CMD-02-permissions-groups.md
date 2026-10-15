# CMD-02 — Permission groups, inheritance, per-world context

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-01`

## Goal
Extend the permission core (CMD-01) with **groups**, **inheritance**, **per-world context** and
meta (prefix/suffix), all resolved on the hot path without a global lock. The LuckPerms-style
management UI/tracks/meta/web editor is a **plugin** (CMD-33); this task is only the storage +
resolution the core and plugins read.

## Model
- A `Group` has an id, a parent group (single inheritance is enough; a set is fine), a priority,
  node grants/denies, and meta (`prefix`, `suffix`, `weight`).
- A player has one or more groups; nodes resolve child-then-parent, highest group priority first.
  Later overrides earlier; an explicit deny always beats a grant.
- `context`: a dimension/world key. A node may carry a context (`false` for "everywhere");
  context-specific matches beat global, and a mismatched context is skipped.
- Storage: reuse the existing world KV (`aether-api` key prefixes) or a `permissions.json` in the
  world dir — mirror however the players/config is stored today, add the prefix to the central key
  list (see T5) and a test that fails on a duplicate prefix.
- The built-in group **`op`** (level 4) is created at startup and receives the `config.operators`
  entries as members (the seed from CMD-01). It is the group the `is_operator` check maps to and it
  cannot be deleted from the UI; a name in the config that is already a member is not duplicated.
- Writes happen off the tick (safe point) and swap in an immutable snapshot; readers take an
  `Arc` snapshot per check. No `Mutex` on the hot path.

## Commands
`/lp` itself is the plugin (CMD-33). This task may expose temporary console-only subcommands for
tests only; do not answer `/lp` from the core.

## Files
- `crates/aether-server/src/permissions.rs` (extend).
- persistence: wherever the server persists small state (grep `put_meta`/player store).

## Edge cases to test explicitly
- child group grant overridden by a player-level deny.
- two groups with conflicting nodes: the higher-priority group wins; equal priority is resolved by
  group id (total, reproducible).
- inheritance cycle is rejected at load, not recursed forever.
- a per-world node applies in that world only; falling back to the global node elsewhere.
- snapshot swap is atomic: a concurrent check never sees a half-applied group edit.
- reload of group storage preserves a player's explicit overrides.

## Done criteria
- Unit tests for resolution, priority tie-break, context, cycle rejection, snapshot atomicity.
- fmt / clippy `-D warnings` / touched-crate tests via `cargo-locked`.
- A short section in `docs/design/` or the module docs describing the on-disk format.
