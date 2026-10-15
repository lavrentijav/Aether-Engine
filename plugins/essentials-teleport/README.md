# essentials-teleport plugin — skeleton (CMD-37)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`) and the core teleport/safe-spot API
(`oc/CMD-31`). **No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-37** — `docs/tasks/CMD-37-essentials-teleport.md`
- Reference: EssentialsX homes/warps/tpa. Overrides core `/spawn` while enabled.

## Commands
| command | syntax | permission |
|---|---|---|
| `/sethome [<name>]`, `/home [<name>]`, `/delhome <name>`, `/homes` | | `essentials.sethome`, `essentials.home`, … |
| `/setwarp <name>`, `/warp <name>`, `/delwarp <name>`, `/warps` | | `essentials.setwarp`, … |
| `/tpa <player>`, `/tpahere <player>`, `/tpaccept`, `/tpdeny`, `/tpcancel` | | `essentials.tpa`, … |
| `/tpall`, `/tphere <player>`, `/tppos <x> <y> <z>` | | `essentials.tpall`, … |
| `/back` | last death/warp/teleport | `essentials.back` |
| `/top`, `/jump`, `/up [<n>]` | | `essentials.top`, … |
| `/rtp [<radius>]` | safe random teleport | `essentials.rtp` |
| `/setspawn` | sets the plugin's spawn (`/spawn` stays core) | `essentials.setspawn` |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `players`, `scheduler`. Subscribes to `on-command`,
`after-entity-death` (for `/back`).

## Notes
Homes/warps/back live in plugin KV; cooldowns and warmups are plugin config (warmup cancels on move
or damage).
