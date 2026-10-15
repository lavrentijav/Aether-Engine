# CMD-12 — V-world: block operations

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`

## Goal
Vanilla world-editing commands. These write blocks, so they must go through the normal placement
path (neighbour updates, light, journal). Use the bulk-edit core API (CMD-30) once it has merged;
until then, batch locally but never bypass the journal.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/setblock <pos> <block> [destroy\|keep\|replace]` | `minecraft.command.setblock` | 2 | `Successfully set block at <x>, <y>, <z> to <block>` |
| `/fill <from> <to> <block> [destroy\|hollow\|keep\|outline\|replace]` | `minecraft.command.fill` | 2 | `Successfully filled <n> blocks` |
| `/fill <from> <to> replace <filter> <block>` | `minecraft.command.fill` | 2 | `Successfully filled <n> blocks` |
| `/clone <begin> <end> <destination> [replace\|masked] [force\|normal] [filtered <filter>]` | `minecraft.command.clone` | 2 | `Successfully cloned <n> blocks` |
| `/clone ... from <dim> ... to <dim> ...` (cross-dimension form) | `minecraft.command.clone` | 2 | same |
| `/fillbiome <from> <to> <biome> [replace <filter>]` | `minecraft.command.fillbiome` | 2 | `Successfully filled <n> blocks with biome <biome>` |
| `/place feature <feature> [<pos>]` | `minecraft.command.place` | 2 | `Placed feature <feature>` |
| `/place jigsaw <pool> <target> <maxDepth> [<pos>]` | `minecraft.command.place` | 2 | `Placed jigsaw <pool>` |
| `/place structure <structure> [<pos>]` | `minecraft.command.place` | 2 | `Placed structure <structure>` |
| `/place template <template> [<pos>] [<rotation>] [<mirror>] [<integrity>] [<seed>]` | `minecraft.command.place` | 2 | `Placed template <template>` |
| `/forceload add <from> [<to>]` | `minecraft.command.forceload` | 2 | `Added <n> force-loaded chunks` |
| `/forceload remove <from> [<to>]` / `remove all` / `query [<pos>]` | `minecraft.command.forceload` | 2 | `Removed <n> force-loaded chunks` / `Force-loaded chunks:` |

Limits: vanilla caps `/fill`/`/clone` at 32768 blocks (`Too many blocks in the specified area`);
block coordinates may not exceed the world height; `destroy` drops items like breaking.

## Files
- new `crates/aether-server/src/commands/world_edit.rs`.
- `crates/aether-server/src/placement.rs`, `game/provenance.rs`, `worldgen` (biome/fillbiome,
  feature/template placement), `aether_world` block state parsing from CMD-06.
- if merged, call `bulk_edit` (CMD-30); the journal must show one grouped operation per command.

## Edge cases to test explicitly
- `/setblock keep` on an occupied cell → `That block is already <block>` and no change/journal.
- `/setblock destroy` on a non-air block drops its items.
- `/fill` hollow vs outline vs replace-filter; 32769 blocks is refused.
- `/clone masked` copies only non-air; `filtered` with a filter argument; `force` overwrites.
- `/fillbiome` with a filter and without; biome is written per cell (heightmap-correct).
- `/place feature` uses the world seed from the data pack, matching worldgen output for a known
  seed/position; a bad template name errors.
- `/forceload` persists across restart; `query` lists the ranges; `remove all` clears.
- permission: level-2 only; a level-3/4 also passes; level 0 cannot.

## Done criteria
- Implementations + tests named above; large-fill benchmark if CMD-30 is in.
- Blocks are visible to clients (chunk resend) and journaled with actor `player:<uuid>`.
- fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
