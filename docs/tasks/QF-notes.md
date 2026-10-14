# Queue F — structure generation notes

## What landed

**F1 — placement** (`crates/aether-worldgen/src/vanilla/structure/`)

* `placement.rs` ports `RandomSpreadStructurePlacement` (spacing, separation,
  `linear`/`triangular`, salt, the four frequency reducers, `locate_offset`,
  exclusion zones) and `ConcentricRingsStructurePlacement`, including the
  `BiomeSource.findBiomeHorizontal` reservoir search behind the 128 stronghold
  rings.
* `mod.rs` reads `worldgen/structure_set/*.json`, pairs each set's placement
  with its weighted structures, keeps only the sets usable by the overworld
  (`hasBiomesForStructureSet`) and reports `starts_at(seed, cx, cz)` with the
  weighted pick. Golden values for the igloo placement (spacing 32 /
  separation 8 / salt 14357618) are checked against a `java.util.Random`
  reference.

**F2 — templates** (`structure/template.rs`, `structure/igloo.rs`, wired into
`generator.rs`)

* `template.rs` parses a `.nbt` palette + block list, applies
  `StructureTemplate.transform` and `BlockState.mirror/rotate` for the property
  shapes the overworld structures use, and places the writes in template order.
* `igloo.rs` ports `IglooStructure` / `IglooPieces`: the above-ground `igloo/top`
  always, a 50 % ladder shaft and laboratory below, each piece snapped to
  `WORLD_SURFACE_WG` at its own corner column and the top's snow lid. The
  `chest` data marker is struck out to air; the chest block stays, empty.
* The generator gates the igloo on the structure's own `biomes` tag (checked at
  the chunk-centre surface quart, `isValidBiome`) and only places when it
  matches.
* Tests: unit tests for `transform` / `transform_state`, and pack-gated
  integration tests that generate a real snowy igloo start and assert the
  template blocks, determinism, and the lab chest.

**F3 — one template loader** (`structure/template.rs`)

* Queue B had grown a second loader (`vanilla/fossil_template.rs`) for the
  fossil feature. Both are now the single `structure/template.rs`:
  `StructureTemplate` + `place_in_world`, the `Nbt` reader, `Rotation`/`Mirror`,
  `zero_position`/`bounding_box`, and the processors (`block_rot`, `rule`,
  `protected_blocks`) live in one place; `fossil_template.rs` is deleted.
* Fossil and igloo both call `place_in_world` with a `PlaceSettings`
  (rotation, mirror, pivot, processors, bounding box, `random_palette`,
  `ignore_structure_blocks`). Fossil keeps `random_palette` set (its
  `StructurePlaceSettings` carries the feature random, so the single-palette
  `nextInt(1)` and the processor draws advance the feature seed) and gets its
  blocks in `buildInfoList` order from `load_from_pack`; the igloo uses file
  order and drops its `structure_block` markers.
* Behaviour is unchanged: the fossil tests and the igloo/parity tests pass
  without edits.

## Accepted F2 deviations

* **Block entities are not written.** A template block with NBT (igloo furnace,
  brewing stand, bed, sign) becomes its block state only; loot tables are not
  assigned, so chests are empty.
* **Multiple palettes are not modelled.** `getRandomPalette` picks the first
  palette; every overworld template that matters here carries one.
* **Waterlogging is not re-applied.** `LiquidSettings.IGNORE_WATERLOGGING`
  means the igloo has nothing to do anyway.
* **Jigsaw blocks are not resolved.** A template containing `jigsaw` blocks
  would place them raw; no shipped template structure used so far has one.
* **Decoration ordering.** Vanilla interleaves structures with the decoration
  steps and later features query the world the structure built. This engine
  computes decoration from the bare terrain, so a feature's write can land on a
  structure cell. Structures are therefore applied **after** decoration, so the
  solid structure blocks win — closest to what a structure-aware feature would
  have produced. (Before this fix a snow layer replaced the igloo's bed.)

## Jigsaw design note (villages) — not implemented in QF

A village is a **jigsaw** structure: instead of one template it grows a tree of
pieces by matching `jigsaw` blocks. The pieces are data-defined.

1. **Data.** `worldgen/structure/village_*.json` names a `start_pool`
   (`minecraft:village/plains/town_centers`), `size` (max depth),
   `start_height`, `project_start_to_heightmap`, `terrain_adaptation`,
   `max_distance_from_center`. `worldgen/template_pool/**` holds the pools:
   each pool element is either `minecraft:single` / `minecraft:list`
   (`element_type`) with a `location`, `processors`, `projection`,
   `override_liquid_settings`, or `minecraft:feature`, plus a `weight`.
2. **Start.** `JigsawStructure.findGenerationPoint` places the first piece at
   the chunk centre, at the heightmap or a fixed `start_height`, then runs
   `JigsawPlacement.addPieces`. A start pool element with `projection=rigid`
   keeps its Y; `terrain_matching` re-snaps each piece.
3. **Growth.** Breadth-first, depth-limited (`size`). Each placed piece exposes
   its `jigsaw` blocks (the NBT carries `name`, `target`, `pool`,
   `final_state`, `joint`). For each free connector the placer draws a pool
   from `pool`, filters elements by `target` ∈ element's own connector names and
   by `canAttach`, applies a mirror/rotation so the connectors face each other,
   and recurses. A candidate is rejected if it leaves `max_distance_from_center`
   or overlaps an existing piece's bounding box (jigsaws may be allowed to
   overlap others' jigsaw blocks).
4. **Determinism.** Piece order and the draws come from the structure's
   `WorldgenRandom` (`setLargeFeatureSeed(seed, cx, cz)`, then feature seeds) and
   from a second random for pool selection. `use_expansion_hack` changes the
   height of village pieces. The exact draw order is the whole game here:
   `JigsawPlacement.Placer.tryPlacingChildren` iterates `StructurePoolElement`
   lists and connector lists in file order and draws one element at a time.
5. **Engine shape.** The natural seam is `structure/`: a `JigsawStructure`
   alongside `igloo.rs`, reusing `template.rs` for each piece plus the
   `jigsaw` NBT (`final_state` replacing the block after placement), the piece
   bounding boxes, the `StructureProcessor` list (`gravity`, `block_ignore`,
   `block_age`, `rule`, `blackstone_replace`, …) and `terrain_adaptation`
   (the `beard_thin` / `beard_box` `WorldGenRegion` passes). Pools and
   processors must be read from the pack at build time like
   `StructureRegistry`, never vendored.
6. **Where it diverges from the current model.** The igloo places a fixed
   layout at one start; a jigsaw needs dynamic, ordered piece growth, a
   bounding-box overlap test, and per-piece processors. The biome gate and
   `starts_at` from F1 are already the right entry point.

## Measured checks

```
AETHER_VANILLA_PACK=<unpacked 1.21.11 jar> \
  cargo test -p aether-worldgen --test structures
```

Seed 42's nearest snowy igloo start is chunk `(-3325, -4091)`; the start's
`igloo/top` is always placed there, the lab chest appears when the 50 % roll
adds it.
