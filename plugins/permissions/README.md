# Permissions plugin — skeleton (CMD-33)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`) and the core permission groups
(`oc/CMD-02`). **No code until P1 is merged** — this directory holds the manifest and this README
only; `entry` is a placeholder.

- CMD task: **CMD-33** — `docs/tasks/CMD-33-permissions-ui.md`
- Reference: LuckPerms-style management UI (tracks, meta, web editor).

## Commands
| command | syntax | permission |
|---|---|---|
| `/lp user <user> …` | `info`, `parent set/add/remove`, `permission set/unset/check/list` | `luckperms.user.*` |
| `/lp group <group> …` | `info`, `parent …`, `permission …` | `luckperms.group.*` |
| `/lp creategroup` / `deletegroup` | `<name>` | `luckperms.creategroup` / `luckperms.deletegroup` |
| `/lp user\|group <t> meta …` | `info`, `set`, `add`, `remove`, `settemp` (prefix/suffix/weight) | `luckperms.meta.*` |
| `/lp track <track> …` | `info`, `create`, `delete`, `append`, `insert`, `remove` | `luckperms.track.*` |
| `/lp editor` | one-time web URL | `luckperms.editor` |
| `/lp sync`, `/lp verbose`, `/lp info` | | `luckperms.sync` etc. |

## Capabilities
`log`, `config`, `supports`, `storage.kv`, `players`. The editor uses the net capability (`PA3`)
when granted and is disabled otherwise.

## Notes
The permission **model** is core (`CMD-01`/`CMD-02`); this plugin owns `/lp` and the UI only. Prefix/
suffix meta is read by the chat plugin (CMD-40).
