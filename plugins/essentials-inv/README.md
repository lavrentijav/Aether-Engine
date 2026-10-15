# essentials-inv plugin — skeleton (CMD-39)

Status: 📋 skeleton (CMD0), **native tier** (in-process, CMD-00N). Needs the native loader (`oc/CMD-00N`) and the core player/window API
(`oc/CMD-32`, PA1). **No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-39** — `docs/tasks/CMD-39-essentials-inv.md`
- Reference: EssentialsX inventory/station access and kits.

## Commands
| command | syntax | permission |
|---|---|---|
| `/invsee <player>` | live window over another inventory | `essentials.invsee` |
| `/enderchest [<player>]` (`/ec`) | | `essentials.enderchest` |
| `/workbench [<player>]` (`/craft`) | | `essentials.workbench` |
| `/anvil`, `/stonecutter`, `/grindstone` | | `essentials.anvil`, … |
| `/trash` (`/disposal`) | | `essentials.trash` |
| `/clearinventory [<player>] [<item>]` (`/ci`) | | `essentials.clearinventory` |
| `/kit [<kit>] [<player>]`, `/kits`, `/createkit <name> [<cooldown>]` | | `essentials.kit`, `essentials.kits`, `essentials.createkit` |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `players`. Subscribes to `on-command`. Kits are read from
`kits.yml` in this directory.

## Notes
`/invsee` and containers use the host window API; on a codec without server-side windows the plugin
degrades with a clear error (decided in CMD-32).
