# CMD-28 — History: restore and purge

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N)
Needs: `oc/CMD-00N`, `oc/CMD-27`

## Goal
Two missing halves of the CoreProtect workflow: `/restore` (re-apply a previous change, the inverse
of `/rollback`) and `/purge` (reclaim disk by deleting old history). `/rollback` already exists and
writes recovered blocks to `/stash`.

**Tier (CMD0 review):** native plugin (in-process Rust), like CMD-27. The core keeps the journal and
the existing rollback; `/restore` `/purge` are added by the native plugin (`needs: oc/CMD-00N`).

## Commands
| syntax | node | op | feedback |
|---|---|---|---|
| `/co restore <filters>` (alias `/restore`) | `aether.command.restore` | 3 | `Restored <n> block(s)` / `nothing matched` |
| `/co purge <filters>` (alias `/purge`) | `aether.command.purge` | 4 | `Purged <n> event(s) older than <time>` |

`/restore` replays the block changes a filter matches **forward** again (the state before the
rollback), journaling the restoration as a new event with actor `player:<uuid>` (or `console`). It
uses the same bounded lookup as `/lookup`; an unrestricted restore must be refused unless an
explicit `time:`/`radius:` limit is present (mirror the `/rollback` guard in `commands/mod.rs`).
`/purge` requires a `time:` filter (CoreProtect requires `t:`); it deletes events older than the
cutoff from the journal/segments, and refuses to run with no filter. Purging is irreversible and
asks for confirmation by default (a `confirm` argument or a two-step prompt); console may skip.

## Files
- `crates/aether-server/src/commands/mod.rs` + `commands/query.rs` (restore/purge).
- `aether-world::journal` for deletion/compaction; storage segments (BACKLOG H2) for efficiency.
- reuse `/stash` deposit from `/rollback` so restored-but-unplaced items are recoverable.

## Edge cases to test explicitly
- restore a change whose post-state no longer matches (someone built over it): decide and document
  (CoreProtect skips or overwrites; choose and test).
- restore is itself journaled so a second rollback can undo it (no infinite loop).
- purge with only a `time:` filter deletes old events; with a `player:` filter keeps others.
- purge with no filter or only `radius:` is refused.
- restore a player's own recent edits is refused when unrestricted.
- purge does not delete events inside the last `time:` window; a checkpoint/segment boundary is
  handled (no half-segment).
- permission: purge level 4, restore level 3.

## Done criteria
- Implementations + tests; a disk-size test that purge actually reclaims.
- fmt / clippy `-D warnings` / `cargo-locked test`.
