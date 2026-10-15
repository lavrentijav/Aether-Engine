# essentials-chat plugin — skeleton (CMD-40)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`), the player API (`oc/CMD-32`) and the
permissions plugin for prefix/suffix meta (`oc/CMD-33`). **No code until P1 is merged** — manifest
and README only.

- CMD task: **CMD-40** — `docs/tasks/CMD-40-essentials-chat.md`
- Reference: EssentialsX private messages, mail, ignore, social spy and chat formatting.

## Commands
| command | syntax | permission |
|---|---|---|
| `/msg <player> <message>` (also core `/tell`/`/w`) | | `essentials.msg` |
| `/r <message>`, `/reply <message>` | reply to the last whisper | `essentials.msg` |
| `/mail send\|read\|clear …` | | `essentials.mail` |
| `/ignore [<player>]` | | `essentials.ignore` |
| `/socialspy` | | `essentials.socialspy` |
| `/broadcast <message>` | | `essentials.broadcast` |
| `/motd`, `/rules` | | `essentials.motd` |
| `/me <action>` | formatting override of core `/me` | `essentials.me` |

## Capabilities / events
`log`, `config`, `supports`, `players`, `storage.kv`. Subscribes to `before-chat`, `on-command`.

## Notes
Formats chat from permission meta (prefix/suffix/colour); interacts with the moderation mute hook
(CMD-41) — hook order is documented there.
