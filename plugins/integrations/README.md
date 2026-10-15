# integrations plugin — skeleton (CMD-45)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`), the net/HTTP capability (`oc/PA3`) and
the player API (`oc/CMD-32`). **No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-45** — `docs/tasks/CMD-45-integrations.md`
- Reference: Discord bridge, NuVotifier, account linking, web stats.

## Commands
| command | syntax | permission |
|---|---|---|
| `/discord [<message>]` | relay to the configured webhook | `integrations.discord` |
| `/vote`, `/vote rewards`, `/votetop` | vote sites + rewards | `integrations.vote` |
| `/link [<code>]`, `/unlink` | account linking | `integrations.link` |
| `/webstats` | post a heartbeat to the web endpoint | `integrations.webstats` |

## Capabilities / events
`log`, `config`, `supports`, `players`, `scheduler`, `net.http`. Subscribes to `on-command`,
`after-player-join`, `before-chat`.

## Notes
All outbound calls are asynchronous through the net capability and never block the tick; the
NuVotifier signature is verified and replayed votes are deduped.
