# CMD-24 — `/aether` admin: reload and backup

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-05`, `oc/CMD-01`, `oc/CMD-21` (reload of data packs)

## Goal
The engine's own operator root `/aether` with partial reload and consistent backups. This is the
parent command that CMD-25 (`profile`) and CMD-26 (`world`) extend.

## Commands
| syntax | node | op | feedback |
|---|---|---|---|
| `/aether reload [config\|permissions\|datapacks\|world]` | `aether.command.reload` | 4 | `Reloaded <part>` |
| `/aether backup [<name>]` | `aether.command.backup` | 4 | `Backup <name> written (<size>)` |
| `/aether` (no subcommand) | — | 4 | usage listing subcommands |

- `/aether reload` with no part reloads config + permissions + data packs; `world` re-reads world
  metadata (spawn, border, gamerules) from disk. Each part is atomic at a safe point — a running
  tick never sees a half-applied reload. A bad config is refused and the old one stays active.
- `/aether backup` takes a **consistent snapshot**: pause/flush the KV store and record the journal
  position; the backup must restore without a journal gap. Reuse the existing storage snapshot
  machinery if present (grep `snapshot`/`backup`); otherwise this task adds it (BACKLOG H8).
  Retention: keep the last N (config).

## Files
- new `crates/aether-server/src/commands/aether.rs` (subcommand router).
- `config.rs` (reload), permissions (CMD-02), data pack loader (CMD-21), `stored.rs` (snapshot).
- register the `/aether` node and subcommand permissions with CMD-03/04.

## Edge cases to test explicitly
- `/aether reload config` with an invalid file keeps the old config and errors.
- reload permissions while a permission check is mid-flight (snapshot swap, no torn read).
- `/aether backup` under concurrent writes: the snapshot is point-in-time consistent (restore test:
  every journaled block change up to the snapshot is present, none after).
- backup names are sanitised; an existing name is not silently overwritten.
- a `world` reload of the border while a player is outside re-applies the border damage rule.
- permission: only level 4; console passes.

## Done criteria
- Implementations + a restore-consistency test.
- fmt / clippy `-D warnings` / `cargo-locked test`.
