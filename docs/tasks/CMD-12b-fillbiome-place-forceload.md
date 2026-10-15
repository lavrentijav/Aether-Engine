# CMD-12b — V-world: fillbiome, place, forceload

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-06b`, `oc/CMD-05`, `oc/CMD-01`
Parent/umbrella: `docs/tasks/CMD-12-block-ops.md`

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/fillbiome <from> <to> <biome> [replace <filter>]` | `minecraft.command.fillbiome` | 2 | `Successfully filled <n> blocks with biome <biome>` |
| `/place feature <feature> [<pos>]` | `minecraft.command.place` | 2 | `Placed feature <feature>` |
| `/place jigsaw <pool> <target> <maxDepth> [<pos>]` | `minecraft.command.place` | 2 | `Placed jigsaw <pool>` |
| `/place structure <structure> [<pos>]` | `minecraft.command.place` | 2 | `Placed structure <structure>` |
| `/place template <template> [<pos>] [<rotation>] [<mirror>] [<integrity>] [<seed>]` | `minecraft.command.place` | 2 | `Placed template <template>` |
| `/forceload add <from> [<to>]` | `minecraft.command.forceload` | 2 | `Added <n> force-loaded chunks` |
| `/forceload remove <from> [<to>]` / `remove all` / `query [<pos>]` | `minecraft.command.forceload` | 2 | `Removed <n> force-loaded chunks` / `Force-loaded chunks:` |

## Files
- new `crates/aether-server/src/commands/world_edit.rs`.
- worldgen (feature/template placement), biome write, forceload set persisted in world metadata.

## Edge cases to test explicitly
- `/fillbiome` with and without a filter; biome written per cell (heightmap-correct).
- `/place feature` uses the world seed and matches worldgen output for a known seed/position; a bad
  template/pool name errors.
- `/forceload` persists across restart; `query` lists ranges; `remove all` clears; a forceloaded
  chunk keeps ticking with no player online.

## Done criteria
- Implementation + tests above; fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
