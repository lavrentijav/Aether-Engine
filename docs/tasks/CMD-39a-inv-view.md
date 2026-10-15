# CMD-39a — Plugin: essentials-inv — inventory views

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/essentials-inv`
Needs: `oc/P1`, `oc/CMD-32b`
Parent/umbrella: `docs/tasks/CMD-39-essentials-inv.md`

## Commands (EssentialsX reference)
| command | syntax | node |
|---|---|---|
| invsee | `/invsee <player>` | `essentials.invsee` |
| enderchest | `/enderchest [<player>]` (`/ec`) | `essentials.enderchest` |
| clear inv | `/clearinventory [<player>] [<item>]` (`/ci`) | `essentials.clearinventory` |

`/invsee` opens a live window over the target's inventory (CMD-32b) — edits affect the owner.

## Files
- `plugins/essentials-inv/`: manifest, `src/`, README.
- window API (CMD-32b); container/inventory model.

## Edge cases to test explicitly
- `/invsee` another player: shift-click/drag validated; edits persist; closing is safe.
- `/ec` of an offline player is refused (no live ender chest).
- `/ci` with an item filter; destruction is journaled (no duplication).
- opening on a codec without server windows: clear error (per CMD-32b).

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
