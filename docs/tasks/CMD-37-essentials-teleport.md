# CMD-37 — Plugin: essentials-teleport

Status: 📋 task brief (CMD0) · size: L · core: no · plugin: `plugins/essentials-teleport`
Needs: `oc/P1`, `oc/CMD-31`

## Goal
The EssentialsX teleport/home/warp family, on the core teleport/safe-spot API (CMD-31) and player KV
(P1). Vanilla has `/spawn` (core, CMD-11); this plugin **overrides** `/spawn` when installed.

## Commands (EssentialsX reference)
| command | syntax | node |
|---|---|---|
| homes | `/sethome [<name>]`, `/home [<name>]`, `/delhome <name>`, `/homes` | `essentials.sethome`, `essentials.home`, … |
| warps | `/setwarp <name>`, `/warp <name>`, `/delwarp <name>`, `/warps` | `essentials.setwarp`, … |
| requests | `/tpa <player>`, `/tpahere <player>`, `/tpaccept [<player>]`, `/tpdeny`, `/tpcancel` | `essentials.tpa`, … |
| ops | `/tpall`, `/tphere <player>`, `/tppos <x> <y> <z>` | `essentials.tpall`, `essentials.tphere`, `essentials.tppos` |
| back | `/back` (last death/warp/teleport) | `essentials.back` |
| vertical | `/top`, `/jump`, `/up [<n>]` | `essentials.top`, `essentials.jump`, `essentials.up` |
| random | `/rtp [<radius>]` (safe spot via CMD-31) | `essentials.rtp` |
| spawn | `/spawn [<player>]`, `/setspawn` (overrides core `/spawn`) | `essentials.spawn` |

Cooldowns and warmups (EssentialsX config: `/tpa` cooldown, `/home` warmup) are plugin config.
Home/warp/back state is per-player/per-server KV. Teleport delays move the player only if they have
not moved/taken damage (EssX semantics). `back` is set on death, on a warmup teleport and on a
`/tpa`.

## Files
- `plugins/essentials-teleport/`: manifest, `src/`, README.
- core teleport + safe spot (CMD-31); KV (P1); death hook (`after-entity-death`) for `/back`;
  move/damage hooks for warmup cancellation.

## Edge cases to test explicitly
- `/sethome` without a bed/anchor; multiple homes up to a permission-based limit; `/delhome` unknown.
- `/home` during warmup: moving cancels; taking damage cancels (EssX).
- `/tpa` to self is refused; `/tpaccept` after the requester went offline; requests expire.
- `/tpahere` teleports the target to the requester.
- `/back` after a death vs after a teleport; `/back` with no history errors.
- `/rtp` finds a safe spot; if none after the budget, errors without teleporting.
- `/top` on a bedrock ceiling; `/jump` with no block ahead no-ops.
- `/spawn` overrides core only while the plugin is enabled; disabling restores core behaviour.
- cooldowns/warmups are bypassable by a permission node (`essentials.<cmd>.bypass`).

## Done criteria
- Plugin + KV persistence + tests; README lists commands and links CMD-37.
- fmt / clippy `-D warnings` / `cargo-locked test`.
