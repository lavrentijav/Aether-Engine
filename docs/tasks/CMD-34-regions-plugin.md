# CMD-34 — Plugin: WorldGuard-style regions

Status: 📋 task brief (CMD0) · size: L · core: no · plugin: `plugins/regions`
Needs: `oc/P1`, `oc/CMD-29`

## Goal
The region **model**, flags and `/rg` commands on top of the core region index + protection check
(CMD-29). This is the WorldGuard half of "core API + plugin": the fast check is core, everything an
operator edits is here.

## Commands (`/rg` = `/region`)
| command | syntax | node |
|---|---|---|
| define | `/rg define <id>` (from a selection, CMD-36 or `//wand`) | `worldguard.region.define` |
| redefine | `/rg redefine <id>` | `worldguard.region.redefine` |
| remove | `/rg remove <id>` | `worldguard.region.remove` |
| flags | `/rg flag <id> <flag> [value]` (pvp, build, use, entry, greeting, …) | `worldguard.region.flag` |
| members | `/rg addmember <id> <player>` / `/rg removemember` | `worldguard.region.member` |
| owners | `/rg addowner <id> <player>` / `/rg removeowner` | `worldguard.region.owner` |
| info | `/rg info [<id>]` | `worldguard.region.info` |
| list | `/rg list [<player>]` | `worldguard.region.list` |
| priority | `/rg priority <id> <n>` | `worldguard.region.priority` |
| bypass | (permission) | `worldguard.region.bypass.<world>` |

Region state persists (name → bounds/priority/flags/members/owners) in plugin KV or a world file;
flags are typed and extend the core's flag registry. Region shapes: cuboid first (polygon optional
later). Operators get bypass; the `build`/`pvp`/`use` flags map to `Action` in CMD-29.

## Files
- `plugins/regions/`: manifest, `src/`, README; selection comes from CMD-36 or a minimal internal
  wand if WorldEdit is not installed.

## Edge cases to test explicitly
- overlapping regions with priorities; a deny in a high-priority region beats an allow below.
- `flag pvp deny` blocks player-vs-player damage but not mob damage.
- addmember/addowner grant build; a non-member is denied, with the configured message.
- region define with a selection spanning chunks; an invalid id errors.
- removing a region takes effect at the next safe point; stored state survives restart.
- a region with no owner is not editable by non-ops.
- `/rg list` shows regions per world; `//wand` selection used by `define` and `redefine`.
- bypass permission is per-world; without it the operator is still protected by flags.

## Done criteria
- Plugin + persistence + tests; README lists commands and links CMD-34.
- fmt / clippy `-D warnings` / `cargo-locked test`.
