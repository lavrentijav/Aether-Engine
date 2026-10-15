# stats plugin — skeleton (CMD-47)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`) and the history read API (`oc/PA2`).
**No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-47** — `docs/tasks/CMD-47-stats.md`
- Reference: `/stats`, `/leaderboard`, `/playtime` reading the journal via PA2.

## Commands
| command | syntax | permission |
|---|---|---|
| `/stats [<player>]` | blocks mined/placed, distance, deaths, kills, playtime | `stats.stats` |
| `/leaderboard <stat> [<page>]` (`/lb`) | | `stats.leaderboard` |
| `/playtime [<player>]`, `/playtime top [<page>]` | | `stats.playtime` |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `players`, `history.read`. Subscribes to `on-command`.

## Notes
Aggregates are cached and refreshed on a schedule, never computed on the hot path; playtime accrues
only while online and is checkpointed on join/leave.
