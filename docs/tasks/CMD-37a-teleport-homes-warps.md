# CMD-37a — Plugin: essentials-teleport — homes, warps, spawn

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/essentials-teleport`
Needs: `oc/P1`, `oc/CMD-31`
Parent/umbrella: `docs/tasks/CMD-37-essentials-teleport.md`

## Goal
Named homes and server warps on the core teleport API (CMD-31) and player KV (P1).

## Commands (EssentialsX reference; new names only)
| command | syntax | node |
|---|---|---|
| homes | `/sethome [<name>]`, `/home [<name>]`, `/delhome <name>`, `/homes` | `essentials.sethome`, `essentials.home`, … |
| warps | `/setwarp <name>`, `/warp <name>`, `/delwarp <name>`, `/warps` | `essentials.setwarp`, … |
| spawn | `/setspawn` (sets the plugin's spawn) | `essentials.setspawn` |

`/spawn` itself stays **core** (CMD-11) and is not overridden; `/setspawn` stores the plugin's spawn
point (see the umbrella's ownership note).

## Edge cases to test explicitly
- `/sethome` without a bed/anchor; multiple homes up to a permission-based limit; `/delhome` unknown.
- `/home` during warmup: moving cancels; taking damage cancels (EssX).
- warp names are sanitised; `/warp` unknown errors; `/setwarp` permission-gated; warmup applies.
- homes/warps persist across restart (KV); a home in an unloaded world loads/teleports via CMD-31.

## Done criteria
- Plugin + KV persistence + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
