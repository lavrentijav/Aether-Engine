# essentials-player plugin — skeleton (CMD-38)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`) and the core player/UI API (`oc/CMD-32`).
**No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-38** — `docs/tasks/CMD-38-essentials-player.md`
- Reference: EssentialsX personal player utilities.

## Commands
| command | syntax | permission |
|---|---|---|
| `/fly [<player>]` | | `essentials.fly` / `.others` |
| `/god [<player>]` | | `essentials.god` |
| `/speed [walk\|fly] <value> [<player>]` | | `essentials.speed` |
| `/feed [<player>]`, `/heal [<player>]` | | `essentials.feed`, `essentials.heal` |
| `/repair [all\|hand\|armor]` | | `essentials.repair` |
| `/hat` | equip the held item | `essentials.hat` |
| `/nick <nickname>`, `/realname <nick>` | | `essentials.nick` |
| `/whois [<player>]`, `/seen <player>` | | `essentials.whois`, `essentials.seen` |
| `/afk [<message>]` | | `essentials.afk` |
| `/near [<radius>]` | | `essentials.near` |
| `/getpos`, `/depth`, `/compass`, `/ptime <time>`, `/pweather <weather>` | | `essentials.getpos`, … |
| `/suicide`, `/burn [<player>] [<seconds>]` | | `essentials.suicide`, `essentials.burn` |
| `/thru`, `/skull [<player>]`, `/more`, `/condense [<item>]`, `/exp [<amount>]` | | `essentials.thru`, … |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `players`, `scheduler`. Subscribes to `on-command`.

## Notes
`/nick` and `/afk` render through the chat plugin (CMD-40); `/ptime`/`/pweather` are per-player
client-side packets.
