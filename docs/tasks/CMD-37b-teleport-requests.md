# CMD-37b — Plugin: essentials-teleport — requests, back, vertical, rtp

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/essentials-teleport`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-31`, `oc/CMD-37a`
Parent/umbrella: `docs/tasks/CMD-37-essentials-teleport.md`

## Goal
Teleport requests, `/back`, vertical helpers and random teleport, on CMD-31 and player KV.

## Commands (EssentialsX reference; new names only)
| command | syntax | node |
|---|---|---|
| requests | `/tpa <player>`, `/tpahere <player>`, `/tpaccept [<player>]`, `/tpdeny`, `/tpcancel` | `essentials.tpa`, … |
| ops | `/tpall`, `/tphere <player>`, `/tppos <x> <y> <z>` | `essentials.tpall`, `essentials.tphere`, `essentials.tppos` |
| back | `/back` (last death/warp/teleport) | `essentials.back` |
| vertical | `/top`, `/jump`, `/up [<n>]` | `essentials.top`, `essentials.jump`, `essentials.up` |
| random | `/rtp [<radius>]` (safe spot via CMD-31) | `essentials.rtp` |

`back` is set on death, on a warmup teleport and on a `/tpa`. Cooldowns/warmups are plugin config,
bypassable by a permission node (`essentials.<cmd>.bypass`).

## Edge cases to test explicitly
- `/tpa` to self is refused; `/tpaccept` after the requester went offline; requests expire.
- `/tpahere` teleports the target to the requester.
- `/back` after a death vs after a teleport; `/back` with no history errors.
- `/rtp` finds a safe spot; if none after the budget, errors without teleporting.
- `/top` on a bedrock ceiling; `/jump` with no block ahead no-ops.
- `/up [<n>]` stops at the first blocked cell; never teleports into a solid block.

## Done criteria
- Plugin + KV persistence + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
