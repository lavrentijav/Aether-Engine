# moderation plugin — skeleton (CMD-41)

Status: 📋 skeleton (CMD0), **native tier** (in-process, CMD-00N). Needs the native loader (`oc/CMD-00N`), the player API (`oc/CMD-32`) and the
chat hook (`oc/CMD-40`). **No code until P1 is merged** — manifest and README only. Size L: split
into `CMD-41a` (mute/warn/chat control) and `CMD-41b` (jail/vanish/freeze/sudo/report) before
assigning.

- CMD task: **CMD-41** — `docs/tasks/CMD-41-moderation.md`
- Reference: EssentialsX moderation family. Core bans/kicks stay in `CMD-08`.

## Commands
| command | syntax | permission |
|---|---|---|
| `/mute`, `/tempmute`, `/unmute` | | `moderation.mute` |
| `/warn`, `/warnings`, `/delwarn` | | `moderation.warn` |
| `/tempban`, `/tempbanip`, `/banip` | | `moderation.tempban` |
| `/jail`, `/unjail`, `/setjail` | | `moderation.jail` |
| `/vanish` (`/v`) | | `moderation.vanish` |
| `/sudo <player> <command>` | run as the target | `moderation.sudo` |
| `/freeze <player>` | | `moderation.freeze` |
| `/clearchat`, `/slowmode`, `/staffchat` (`/sc`) | | `moderation.clearchat`, … |
| `/report`, `/reports` | | `moderation.report` |
| `/history <player>` | bans/kicks/mutes/warns on record | `moderation.history` |

## Capabilities / events
`log`, `config`, `supports`, `players`, `storage.kv`, `scheduler`. Subscribes to `before-chat`,
`on-command`, `after-player-join`, `after-player-leave`, `after-entity-death`.

## Notes
Temporary actions expire via the host scheduler and persist across restart; `/history` merges core
ban records (CMD-08).
