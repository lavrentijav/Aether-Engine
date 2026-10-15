# fun plugin — skeleton (CMD-46)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`) and the player/UI API (`oc/CMD-32`).
**No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-46** — `docs/tasks/CMD-46-fun.md`
- Reference: cosmetic/convenience commands (firework, smite, trees, books, powertools, pets).

## Commands
| command | syntax | permission |
|---|---|---|
| `/firework [<color>] [<shape>] [<trail>] [<flicker>]` | | `fun.firework` |
| `/lightning [<player>]` (`/smite`) | | `fun.lightning` |
| `/tree <type>`, `/bigtree <type>` | | `fun.tree` |
| `/book title\|author\|give …` | | `fun.book` |
| `/powertool <command>` / `/powertool` | bind the held item | `fun.powertool` |
| `/particles [<effect>] [<player>]` | | `fun.particles` |
| `/pet [<type>]`, `/pet remove` | | `fun.pet` |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `players`, `world.read`, `world.write`. Subscribes to
`on-command`, `before-use-item` (powertool).

## Notes
`/tree` reuses the worldgen tree features; `/lightning` spawns a real bolt. Powertools run as the
player and are permission-checked.
