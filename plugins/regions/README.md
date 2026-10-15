# Regions plugin — skeleton (CMD-34)

Status: 📋 skeleton (CMD0), **native tier** (in-process, CMD-00N). Needs the native loader (`oc/CMD-00N`) and the core region index/protection
check (`oc/CMD-29`). **No code until P1 is merged** — manifest and README only.

- CMD task: **CMD-34** — `docs/tasks/CMD-34-regions-plugin.md`
- Reference: WorldGuard regions. The fast check is core; this plugin is the region model, flags and
  `/rg` commands.

## Commands (`/rg` = `/region`)
| command | syntax | permission |
|---|---|---|
| `/rg define <id>` | from the current selection | `worldguard.region.define` |
| `/rg redefine <id>` | from the current selection | `worldguard.region.redefine` |
| `/rg remove <id>` | | `worldguard.region.remove` |
| `/rg flag <id> <flag> [value]` | `pvp`, `build`, `use`, `entry`, `greeting`, … | `worldguard.region.flag` |
| `/rg addmember\|removemember <id> <player>` | | `worldguard.region.member` |
| `/rg addowner\|removeowner <id> <player>` | | `worldguard.region.owner` |
| `/rg info [<id>]`, `/rg list [<player>]` | | `worldguard.region.info` / `.list` |
| `/rg priority <id> <n>` | | `worldguard.region.priority` |
| bypass | (permission) | `worldguard.region.bypass.<world>` |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `world.read`, `players`. Subscribes to
`before-block-break`, `before-block-place`, `before-use-item`, `before-attack`, `on-command`.

## Notes
Selection comes from the WorldEdit plugin (CMD-36) or a minimal internal wand. Region state persists
in plugin KV. Cuboid regions first; polygon later.
