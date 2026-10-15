# CMD-34a — Plugin: regions model & shape commands

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/regions`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-29b`
Parent/umbrella: `docs/tasks/CMD-34-regions-plugin.md`

## Commands (`/rg` = `/region`)
| command | syntax | node |
|---|---|---|
| define | `/rg define <id>` (from the selection) | `worldguard.region.define` |
| redefine | `/rg redefine <id>` | `worldguard.region.redefine` |
| remove | `/rg remove <id>` | `worldguard.region.remove` |
| priority | `/rg priority <id> <n>` | `worldguard.region.priority` |
| info/list | `/rg info [<id>]`, `/rg list [<player>]` | `worldguard.region.info` / `.list` |

## Files
- `plugins/regions/`: manifest, `src/`, README.
- region index/check (CMD-29a/29b); selection from WorldEdit (CMD-36) or a minimal internal wand.
- region state persists in plugin KV (cuboid first; polygon later).

## Edge cases to test explicitly
- define with a selection spanning chunks; an invalid id errors; redefine moves bounds atomically.
- `/rg list` shows regions per world; removing a region takes effect at the next safe point.
- a region with no owner is not editable by non-ops; state survives restart.

## Done criteria
- Plugin + persistence + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
