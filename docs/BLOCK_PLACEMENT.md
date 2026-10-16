# Block placement rules

What may be placed where, the replaceable list, and the reverse "what supports what" index. Generated from `crates/aether-server/src/placement`; **do not edit by hand**.

Legend: source **data** = the block set comes from the vanilla data pack; **curated** = the rule shape is ported from vanilla's block classes.

Regenerate with `cargo run -p aether-server --example gen_placement_md`.

Replaceable list source: **the compiled-in fallback** (29 blocks).

## Replaceable blocks

- `minecraft:air`
- `minecraft:bubble_column`
- `minecraft:bush`
- `minecraft:cave_air`
- `minecraft:crimson_roots`
- `minecraft:dead_bush`
- `minecraft:fern`
- `minecraft:fire`
- `minecraft:glow_lichen`
- `minecraft:hanging_roots`
- `minecraft:large_fern`
- `minecraft:lava`
- `minecraft:leaf_litter`
- `minecraft:light`
- `minecraft:nether_sprouts`
- `minecraft:resin_clump`
- `minecraft:seagrass`
- `minecraft:short_dry_grass`
- `minecraft:short_grass`
- `minecraft:snow`
- `minecraft:soul_fire`
- `minecraft:structure_void`
- `minecraft:tall_dry_grass`
- `minecraft:tall_grass`
- `minecraft:tall_seagrass`
- `minecraft:vine`
- `minecraft:void_air`
- `minecraft:warped_roots`
- `minecraft:water`

## Rules by group

### Vegetation on soil

Vanilla `VegetationBlock.canSurvive`: the block below must be in `#minecraft:dirt` or be farmland. A flower, sapling or grass on sand, stone or air is refused.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `short_grass`, `fern`, `bush`, `firefly_bush`, `sweet_berry_bush`, `pink_petals`, `wildflowers`, `dandelion`, `poppy`, `blue_orchid`, `allium`, `azure_bluet`, `red_tulip`, `orange_tulip`, `white_tulip`, `pink_tulip`, `oxeye_daisy`, `cornflower`, `lily_of_the_valley`, `torchflower`, `open_eyeblossom`, `closed_eyeblossom`, `oak_sapling`, `spruce_sapling`, `birch_sapling`, `jungle_sapling`, `acacia_sapling`, `cherry_sapling`, `dark_oak_sapling`, `pale_oak_sapling` | on the block below | minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:rooted_dirt, minecraft:farmland | data |

### Species with their own soil

Wither rose also grows on netherrack and soul soil; azalea also on clay. From the same classes' overridden `mayPlaceOn`.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `wither_rose` | on the block below | minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:rooted_dirt, minecraft:farmland, minecraft:netherrack, minecraft:soul_sand, minecraft:soul_soil | data |
| `azalea`, `flowering_azalea` | on the block below | minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:rooted_dirt, minecraft:farmland, minecraft:clay | data |

### Dry vegetation

`#minecraft:dry_vegetation_may_place_on`: sand, terracotta, dirt or farmland.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `dead_bush`, `short_dry_grass`, `tall_dry_grass` | on the block below | minecraft:black_terracotta, minecraft:blue_terracotta, minecraft:brown_terracotta, minecraft:coarse_dirt, minecraft:cyan_terracotta, minecraft:dirt, minecraft:farmland, minecraft:grass_block, minecraft:gray_terracotta, minecraft:green_terracotta, minecraft:light_blue_terracotta, minecraft:light_gray_terracotta, minecraft:lime_terracotta, minecraft:magenta_terracotta, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:orange_terracotta, minecraft:pale_moss_block, minecraft:pink_terracotta, minecraft:podzol, minecraft:purple_terracotta, minecraft:red_sand, minecraft:red_terracotta, minecraft:rooted_dirt, minecraft:sand, minecraft:suspicious_sand, minecraft:terracotta, minecraft:white_terracotta, minecraft:yellow_terracotta | data |

### Two-block plants (lower half)

The lower half follows the soil rule; the upper half is placed with it and needs the lower half below (`placement::upper_half_of`).

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `tall_grass`, `large_fern`, `sunflower`, `lilac`, `rose_bush`, `peony`, `pitcher_plant` | on the block below | minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:rooted_dirt, minecraft:farmland | data |

### Cactus

`CactusBlock.canSurvive`: sand (or another cactus) below, no solid or lava neighbour, and nothing fluid above.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `cactus` | on sand, clear sides, no fluid above | #minecraft:sand, or another cactus | curated |

### Sugar cane

`SugarCaneBlock.canSurvive`: dirt or sand below (or another cane), with water or frosted ice touching a side of that block.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `sugar_cane` | on dirt/sand with water beside | #minecraft:dirt or #minecraft:sand, with water beside | data |

### Bamboo

`#minecraft:bamboo_plantable_on`: sand, dirt, gravel, bamboo.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `bamboo`, `bamboo_sapling` | on the block below | minecraft:bamboo, minecraft:bamboo_sapling, minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:gravel, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:red_sand, minecraft:rooted_dirt, minecraft:sand, minecraft:suspicious_gravel, minecraft:suspicious_sand | data |

### Water plants and corals

Sea grass, kelp and corals stand on a full block; tall sea grass and small dripleaf's submerged form also require a water source in the cell; lily pads float on water or ice.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `seagrass` | on the block below | any full block | curated |
| `tall_seagrass` | on the block below, in a water source | any full block | curated |
| `kelp`, `kelp_plant` | on the block below, or the same block | any full block | curated |
| `lily_pad` | on water or ice, not in a fluid | water or #minecraft:ice | curated |
| `sea_pickle` | on the block below | any non-air block | curated |
| `tube_coral`, `brain_coral`, `bubble_coral`, `fire_coral`, `horn_coral`, `tube_coral_fan`, `brain_coral_fan`, `bubble_coral_fan`, `fire_coral_fan`, `horn_coral_fan`, `dead_tube_coral`, `dead_brain_coral`, `dead_bubble_coral`, `dead_fire_coral`, `dead_horn_coral` | on the block below | any full block | curated |
| `big_dripleaf`, `big_dripleaf_stem` | on the block below, or the same block | minecraft:clay, minecraft:coarse_dirt, minecraft:dirt, minecraft:farmland, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:podzol, minecraft:rooted_dirt | data |

### Small dripleaf

`SmallDripleafBlock.canSurvive`: `#minecraft:small_dripleaf_placeable` (clay or moss) below, or a water source in the cell over a full block.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `small_dripleaf` | on the tag below, or in a water source | minecraft:clay, minecraft:moss_block | data |

### Ceiling plants

Hanging roots, spore blossoms and cave vines hang from a full block (or from another block of the same kind) above them.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `hanging_roots`, `spore_blossom` | under the block above | any full block | curated |
| `cave_vines`, `cave_vines_plant` | under the block above, or the same block | any full block | curated |
| `pale_hanging_moss` | under the block above, or the same block | any full block | curated |

### Carpets, moss and leaf litter

Moss and pale moss carpet sit on any non-air block. `leaf_litter` is the reported bug: it must sit on a full block **and** must never be placed into water — two independent checks.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `moss_carpet`, `pale_moss_carpet` | on the block below | any non-air block | curated |
| `leaf_litter` | on the block below, and not in water | any full block | curated |

### Cocoa

`CocoaBlock.canSurvive`: the block in the direction the pod faces is a jungle log.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `cocoa` | against the block it faces | minecraft:jungle_log, minecraft:jungle_wood, minecraft:stripped_jungle_log, minecraft:stripped_jungle_wood | data |

### Snow layer

`SnowLayerBlock.canSurvive`: refused on ice/packed ice/barrier, always allowed on honey block/soul sand/mud, otherwise a full block below.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `snow` | as the snow-layer rule allows | a full block, honey, soul sand or mud | data |

### Blocks standing on a support

A standing torch uses a support centre below (`BaseTorchBlock`); a lantern hangs or stands by its `hanging` property; a rail lies on a full block.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `torch`, `soul_torch`, `redstone_torch`, `copper_torch` | on a support below, or the wall clicked | a full block or the clicked wall | curated |
| `lantern`, `soul_lantern` | on the block below | any full block | curated |
| `cactus_flower` | on the block below, or the same block | any full block | curated |

### Wall-mounted blocks

`WallTorchBlock`/`LadderBlock`: the block behind the block's `facing` must offer a full face.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| any `*wall_torch` | against the block behind its facing | any full block | curated |
| `ladder` | against the block behind its facing | any full block | curated |
| any `*_wall_sign`, any `*_wall_banner` | against the block behind its facing | any full block | curated |

### Rails

Every rail needs a full block below.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `rail`, `powered_rail`, `detector_rail`, `activator_rail` | on the block below | any full block | curated |

### Blocks on any face

`MultifaceBlock`/`VineBlock`: glow lichen, resin clump and vines attach to a full face of any neighbour.

| Block(s) | Constraint | Valid support | Source |
|---|---|---|---|
| `vine`, `glow_lichen`, `resin_clump` | a full face on any neighbour | full face | curated |

## Reverse index — what supports what

- **below #minecraft:bamboo_plantable_on** — bamboo, bamboo_sapling
- **below #minecraft:big_dripleaf_placeable** — big_dripleaf, big_dripleaf_stem
- **below #minecraft:dirt or #minecraft:sand, with water beside** — sugar_cane
- **below #minecraft:dirt or farmland** — acacia_sapling, allium, azalea, azure_bluet, birch_sapling, blue_orchid, bush, cherry_sapling, closed_eyeblossom, cornflower, dandelion, dark_oak_sapling, fern, firefly_bush, flowering_azalea, jungle_sapling, large_fern, lilac, lily_of_the_valley, oak_sapling, open_eyeblossom, orange_tulip, oxeye_daisy, pale_oak_sapling, peony, pink_petals, pink_tulip, pitcher_plant, poppy, red_tulip, rose_bush, short_grass, spruce_sapling, sunflower, sweet_berry_bush, tall_grass, torchflower, white_tulip, wildflowers, wither_rose
- **below #minecraft:dry_vegetation_may_place_on** — dead_bush, short_dry_grass, tall_dry_grass
- **below #minecraft:sand, or another cactus** — cactus
- **below #minecraft:small_dripleaf_placeable** — small_dripleaf
- **below a full block** — activator_rail, brain_coral, brain_coral_fan, bubble_coral, bubble_coral_fan, cactus_flower, dead_brain_coral, dead_bubble_coral, dead_fire_coral, dead_horn_coral, dead_tube_coral, detector_rail, fire_coral, fire_coral_fan, horn_coral, horn_coral_fan, kelp, kelp_plant, lantern, leaf_litter, powered_rail, rail, seagrass, soul_lantern, tall_seagrass, tube_coral, tube_coral_fan
- **below a full block or the clicked wall** — copper_torch, redstone_torch, soul_torch, torch
- **below a full block, honey, soul sand or mud** — snow
- **below any non-air block** — moss_carpet, pale_moss_carpet, sea_pickle
- **below water or #minecraft:ice** — lily_pad
- **above a full block** — cave_vines, cave_vines_plant, hanging_roots, pale_hanging_moss, spore_blossom
- **behind a full block** — *_wall_banner, *_wall_sign, *wall_torch, ladder
- **in front #minecraft:jungle_logs** — cocoa
- **any adjacent full face** — glow_lichen, resin_clump, vine

