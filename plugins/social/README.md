# social plugin — skeleton (CMD-44)

Status: 📋 skeleton (CMD0), **WASM tier** (sandboxed). Needs the plugin host (`oc/P1`), the player API (`oc/CMD-32`) and the
teleport plugin/API (`oc/CMD-37`, `oc/CMD-31`). **No code until P1 is merged** — manifest and README
only.

- CMD task: **CMD-44** — `docs/tasks/CMD-44-social.md`
- Reference: parties and friends for group play.

## Commands
| command | syntax | permission |
|---|---|---|
| `/party create\|invite\|accept\|deny\|leave\|kick\|disband\|info` | | `social.party` |
| `/party chat <message>` (`/pc`) | | `social.party.chat` |
| `/tpparty` (`/pt`) | teleport the party | `social.party.teleport` |
| `/friend add\|remove\|list\|requests\|accept` | | `social.friend` |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `players`. Subscribes to `on-command`.

## Notes
Friendships are mutual and persist in player KV; a party is transient session state. `/tpparty`
respects region entry (CMD-29/34).
