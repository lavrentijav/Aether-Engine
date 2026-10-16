# Правила установки блоков

Что куда можно ставить, список заменяемых блоков и обратный индекс «что поддерживает что». Сгенерировано из `crates/aether-server/src/placement`; **не редактировать вручную**.

Легенда: источник **data** — набор блоков взят из ванильного пакета данных; **curated** — форма правила перенесена из классов блоков ванилы.

Перегенерировать: `cargo run -p aether-server --example gen_placement_md`.

Replaceable list source: **the compiled-in fallback** (29 blocks).

## Заменяемые блоки

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

## Правила по группам

### Растительность на почве

Ванильный `VegetationBlock.canSurvive`: блок снизу должен входить в `#minecraft:dirt` или быть грядкой. Цветок, саженец или трава на песке, камне или в воздухе отклоняются.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `short_grass`, `fern`, `bush`, `firefly_bush`, `sweet_berry_bush`, `pink_petals`, `wildflowers`, `dandelion`, `poppy`, `blue_orchid`, `allium`, `azure_bluet`, `red_tulip`, `orange_tulip`, `white_tulip`, `pink_tulip`, `oxeye_daisy`, `cornflower`, `lily_of_the_valley`, `torchflower`, `open_eyeblossom`, `closed_eyeblossom`, `oak_sapling`, `spruce_sapling`, `birch_sapling`, `jungle_sapling`, `acacia_sapling`, `cherry_sapling`, `dark_oak_sapling`, `pale_oak_sapling` | на блок снизу | minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:rooted_dirt, minecraft:farmland | data |

### Виды со своей почвой

Роза иссушения также растёт на незераке и душевной земле; азалия — на глине. Из переопределённого `mayPlaceOn` тех же классов.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `wither_rose` | на блок снизу | minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:rooted_dirt, minecraft:farmland, minecraft:netherrack, minecraft:soul_sand, minecraft:soul_soil | data |
| `azalea`, `flowering_azalea` | на блок снизу | minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:rooted_dirt, minecraft:farmland, minecraft:clay | data |

### Сухая растительность

`#minecraft:dry_vegetation_may_place_on`: песок, терракота, земля или грядка.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `dead_bush`, `short_dry_grass`, `tall_dry_grass` | на блок снизу | minecraft:black_terracotta, minecraft:blue_terracotta, minecraft:brown_terracotta, minecraft:coarse_dirt, minecraft:cyan_terracotta, minecraft:dirt, minecraft:farmland, minecraft:grass_block, minecraft:gray_terracotta, minecraft:green_terracotta, minecraft:light_blue_terracotta, minecraft:light_gray_terracotta, minecraft:lime_terracotta, minecraft:magenta_terracotta, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:orange_terracotta, minecraft:pale_moss_block, minecraft:pink_terracotta, minecraft:podzol, minecraft:purple_terracotta, minecraft:red_sand, minecraft:red_terracotta, minecraft:rooted_dirt, minecraft:sand, minecraft:suspicious_sand, minecraft:terracotta, minecraft:white_terracotta, minecraft:yellow_terracotta | data |

### Двухблочные растения (нижняя половина)

Нижняя половина подчиняется правилу почвы; верхняя ставится вместе с ней и требует нижнюю половину снизу (`placement::upper_half_of`).

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `tall_grass`, `large_fern`, `sunflower`, `lilac`, `rose_bush`, `peony`, `pitcher_plant` | на блок снизу | minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:rooted_dirt, minecraft:farmland | data |

### Кактус

`CactusBlock.canSurvive`: под ним песок (или кактус), рядом нет твёрдого блока или лавы, сверху нет жидкости.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `cactus` | на песок, свободные бока, без жидкости сверху | #minecraft:sand или другой кактус | curated |

### Сахарный тростник

`SugarCaneBlock.canSurvive`: снизу земля или песок (или тростник), и вода либо замороженный лёд касается бока этого блока.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `sugar_cane` | на землю/песок с водой рядом | #minecraft:dirt или #minecraft:sand, вода рядом | data |

### Бамбук

`#minecraft:bamboo_plantable_on`: песок, земля, гравий, бамбук.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `bamboo`, `bamboo_sapling` | на блок снизу | minecraft:bamboo, minecraft:bamboo_sapling, minecraft:coarse_dirt, minecraft:dirt, minecraft:grass_block, minecraft:gravel, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:pale_moss_block, minecraft:podzol, minecraft:red_sand, minecraft:rooted_dirt, minecraft:sand, minecraft:suspicious_gravel, minecraft:suspicious_sand | data |

### Водные растения и кораллы

Морской траве, ламинарии и высокой морской траве нужен **источник** воды в клетке (`getStateForPlacement` без него возвращает null) и полный блок (не магма) снизу; кораллы стоят на полном блоке; кувшинка плавает на воде или льду; морскому огурцу нужна грань коллизии снизу.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `seagrass` | на блок снизу, в источнике воды | any full block | curated |
| `tall_seagrass` | на блок снизу, в источнике воды | any full block | curated |
| `kelp`, `kelp_plant` | на блок снизу или такой же блок, в источнике воды | any full block | curated |
| `lily_pad` | на воду или лёд, не в жидкости | вода или #minecraft:ice | curated |
| `sea_pickle` | на блок снизу | any collidable block | curated |
| `tube_coral`, `brain_coral`, `bubble_coral`, `fire_coral`, `horn_coral`, `tube_coral_fan`, `brain_coral_fan`, `bubble_coral_fan`, `fire_coral_fan`, `horn_coral_fan`, `dead_tube_coral`, `dead_brain_coral`, `dead_bubble_coral`, `dead_fire_coral`, `dead_horn_coral` | на блок снизу | any full block | curated |
| `big_dripleaf`, `big_dripleaf_stem` | на блок снизу или такой же блок | minecraft:clay, minecraft:coarse_dirt, minecraft:dirt, minecraft:farmland, minecraft:grass_block, minecraft:moss_block, minecraft:mud, minecraft:muddy_mangrove_roots, minecraft:mycelium, minecraft:podzol, minecraft:rooted_dirt | data |

### Малый диплеф

`SmallDripleafBlock.mayPlaceOn`: снизу `#minecraft:small_dripleaf_placeable` (глина или мох), либо источник воды в клетке над `#minecraft:dirt` или грядкой.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `small_dripleaf` | на тег снизу или в источнике воды над почвой | minecraft:clay, minecraft:moss_block | data |

### Потолочные растения

Висячие корни, спороцвет и пещерные лозы свисают с полного блока (или с такого же блока) сверху.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `hanging_roots`, `spore_blossom` | под блоком сверху | any full block | curated |
| `cave_vines`, `cave_vines_plant` | под блоком сверху или таким же блоком | any full block | curated |
| `pale_hanging_moss` | под блоком сверху или таким же блоком | any full block | curated |

### Ковры, мох и листва

Моховый и бледно-моховый ковёр кладутся на любой не-воздух. `leaf_litter` — это сообщённый баг: он должен лежать на полном блоке **и** никогда не ставиться в воду — две независимые проверки.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `moss_carpet`, `pale_moss_carpet` | на блок снизу | any non-air block | curated |
| `leaf_litter` | на блок снизу и не в воде | any full block | curated |

### Какао

`CocoaBlock.canSurvive`: блок в направлении, куда смотрит стручок — бревно тропического дерева.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `cocoa` | к блоку, на который смотрит | minecraft:jungle_log, minecraft:jungle_wood, minecraft:stripped_jungle_log, minecraft:stripped_jungle_wood | data |

### Слой снега

`SnowLayerBlock.canSurvive`: нельзя на льду/плотном льду/барьере, всегда можно на медовом блоке/душевном песке/грязи, иначе — полный блок снизу.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `snow` | как допускает правило слоя снега | полный блок, мёд, душевный песок или грязь | data |

### Блоки, стоящие на опоре

Стоящий факел опирается на центр опоры снизу (`BaseTorchBlock`); фонарь висит или стоит по свойству `hanging`; рельсы лежат на полном блоке.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `torch`, `soul_torch`, `redstone_torch`, `copper_torch` | на опору снизу или на стену, куда кликнули | полный блок или стена, куда кликнули | curated |
| `lantern`, `soul_lantern` | на блок снизу | any full block | curated |
| `cactus_flower` | на блок снизу или такой же блок | any full block | curated |

### Настенные блоки

`WallTorchBlock`/`LadderBlock`: блок позади `facing` должен давать полную грань.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| any `*wall_torch` | к блоку позади facing | any full block | curated |
| `ladder` | к блоку позади facing | any full block | curated |
| any `*_wall_sign`, any `*_wall_banner` | к блоку позади facing | any full block | curated |

### Рельсы

Любым рельсам нужен полный блок снизу.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `rail`, `powered_rail`, `detector_rail`, `activator_rail` | на блок снизу | any full block | curated |

### Блоки на любой грани

`MultifaceBlock`/`VineBlock`: светящийся лишайник, смоляной комок и лозы крепятся к полной грани любого соседа.

| Блок(и) | Ограничение | Допустимая опора | Источник |
|---|---|---|---|
| `vine`, `glow_lichen`, `resin_clump` | полная грань любого соседа | полная грань | curated |

## Обратный индекс — что поддерживает что

- **снизу #minecraft:bamboo_plantable_on** — bamboo, bamboo_sapling
- **снизу #minecraft:big_dripleaf_placeable** — big_dripleaf, big_dripleaf_stem
- **снизу #minecraft:dirt or farmland** — acacia_sapling, allium, azalea, azure_bluet, birch_sapling, blue_orchid, bush, cherry_sapling, closed_eyeblossom, cornflower, dandelion, dark_oak_sapling, fern, firefly_bush, flowering_azalea, jungle_sapling, large_fern, lilac, lily_of_the_valley, oak_sapling, open_eyeblossom, orange_tulip, oxeye_daisy, pale_oak_sapling, peony, pink_petals, pink_tulip, pitcher_plant, poppy, red_tulip, rose_bush, short_grass, spruce_sapling, sunflower, sweet_berry_bush, tall_grass, torchflower, white_tulip, wildflowers, wither_rose
- **снизу #minecraft:dirt или #minecraft:sand, вода рядом** — sugar_cane
- **снизу #minecraft:dry_vegetation_may_place_on** — dead_bush, short_dry_grass, tall_dry_grass
- **снизу #minecraft:sand или другой кактус** — cactus
- **снизу #minecraft:small_dripleaf_placeable** — small_dripleaf
- **снизу a full block** — activator_rail, brain_coral, brain_coral_fan, bubble_coral, bubble_coral_fan, cactus_flower, dead_brain_coral, dead_bubble_coral, dead_fire_coral, dead_horn_coral, dead_tube_coral, detector_rail, fire_coral, fire_coral_fan, horn_coral, horn_coral_fan, kelp, kelp_plant, lantern, leaf_litter, powered_rail, rail, seagrass, soul_lantern, tall_seagrass, tube_coral, tube_coral_fan
- **снизу any collidable block** — sea_pickle
- **снизу any non-air block** — moss_carpet, pale_moss_carpet
- **снизу вода или #minecraft:ice** — lily_pad
- **снизу полный блок или стена, куда кликнули** — copper_torch, redstone_torch, soul_torch, torch
- **снизу полный блок, мёд, душевный песок или грязь** — snow
- **сверху a full block** — cave_vines, cave_vines_plant, hanging_roots, pale_hanging_moss, spore_blossom
- **сзади a full block** — *_wall_banner, *_wall_sign, *wall_torch, ladder
- **спереди #minecraft:jungle_logs** — cocoa
- **любая грань полная грань** — glow_lichen, resin_clump, vine

