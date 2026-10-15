# Claims plugin — skeleton (CMD-35)

Status: 📋 skeleton (CMD0). Needs the plugin host (`oc/P1`) and the core region index/protection
check (`oc/CMD-29`). **No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-35** — `docs/tasks/CMD-35-claims-plugin.md`
- Reference: GriefPrevention claims; golden-shovel claiming on the same core region API.

## Commands
| command | syntax | permission |
|---|---|---|
| `/claim` | create/inspect a claim | `griefprevention.claim` |
| `/claim <radius>` | square claim | `griefprevention.claim` |
| `/abandonclaim` / `/abandonallclaims` | give a claim (or all) up | `griefprevention.abandonclaim` / `.abandonallclaims` |
| `/trust <player>` / `/untrust <player>` | build/container/access trust | `griefprevention.trust` |
| `/claimslist` | list your claims | `griefprevention.claimslist` |
| golden shovel | right-click ground to claim, a claim to inspect | — |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `world.read`. Subscribes to `before-block-break`,
`before-block-place`, `before-use-item`, `before-container-click`, `on-command`.

## Notes
Same region check as WorldGuard regions (CMD-34); precedence between a claim and a region follows
region priority and is decided in CMD-35.
