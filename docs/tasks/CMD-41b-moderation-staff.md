# CMD-41b — Plugin: moderation — jail, vanish, freeze, sudo, report, history

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/moderation`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-32a`, `oc/CMD-40`, `oc/CMD-41a`
Parent/umbrella: `docs/tasks/CMD-41-moderation.md`

## Commands
| command | syntax | node |
|---|---|---|
| jail | `/jail <player> [<area>]`, `/unjail <player>`, `/setjail <name>` | `moderation.jail` |
| vanish | `/vanish` (`/v`) | `moderation.vanish` |
| sudo | `/sudo <player> <command>` | `moderation.sudo` |
| freeze | `/freeze <player> [<reason>]` | `moderation.freeze` |
| reports | `/report <player> <reason>`, `/reports`, `/report <id> <action>` | `moderation.report` |
| history | `/history <player>` (bans/kicks/mutes/warns on record) | `moderation.history` |

`/vanish` hides the player from non-staff (tab + entity tracking) and blocks mob targeting.
`/freeze` prevents movement. `/sudo` runs a command **as the target with the target's permissions**
— never as console.

## Files
- `plugins/moderation/`: manifest, `src/`, README.
- player API (CMD-32a) for vanish/freeze; P1 scheduler; records persistence from CMD-41a.

## Edge cases to test explicitly
- `/jail` in another world; `/unjail` with a full inventory.
- `/vanish`: hidden from non-staff, visible to staff; mobs ignore a vanished player.
- `/freeze` prevents movement but not chat; `/freeze` again unfreezes.
- `/sudo` a command the target cannot run is denied as the target (no console escalation).
- `/report` persists, `/reports` lists, handled reports are marked; `/history` merges core bans
  (CMD-08) and plugin records.

## Done criteria
- Plugin + persistence + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
