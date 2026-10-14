//! Configured features: parsing and placement, ported from the game's
//! `Feature` subclasses. Each `place_*` draws from the random source in the
//! same order as its Java counterpart.

use std::sync::{Arc, OnceLock};

use aether_world::BlockStateId;

use super::super::blockinfo::{self, prop};
use super::super::density::BuildError;
use super::super::json::Json;
use super::super::noise::NormalNoise;
use super::super::providers::IntProvider;
use super::super::rng::Rng;
use super::super::structure::{
    self, bounding_box, zero_position, Processor, Rotation, StructureTemplate,
};
use super::super::surface::parse_block_state;
use super::super::tags::BlockSet;
use super::blocks::{
    can_survive, is_water_source, legacy_solid, solid_render, sturdy, BlockPredicate, RuleTest,
    StateProvider,
};
use super::level::{Dir, Heightmap, Level, Pos};
use super::tree::{self, FallenTree, TreeConfig};
use super::{place_placed, Ctx, Loader, Placed};

/// One ore target.
#[derive(Debug)]
pub struct OreTarget {
    test: RuleTest,
    state: BlockStateId,
}

/// A `geode` configuration.
#[derive(Debug)]
pub struct GeodeConfig {
    filling: StateProvider,
    inner: StateProvider,
    alternate_inner: StateProvider,
    middle: StateProvider,
    outer: StateProvider,
    inner_placements: Vec<BlockStateId>,
    cannot_replace: BlockSet,
    invalid_blocks: BlockSet,
    filling_layer: f64,
    inner_layer: f64,
    middle_layer: f64,
    outer_layer: f64,
    crack_chance: f64,
    crack_base: f64,
    crack_offset: i32,
    use_potential: f64,
    use_alternate: f64,
    require_alternate: bool,
    outer_wall: IntProvider,
    distribution: IntProvider,
    point_offset: IntProvider,
    min_gen_offset: i32,
    max_gen_offset: i32,
    noise_multiplier: f64,
    invalid_threshold: i32,
}

/// A `fossil` configuration: a base template and an overlay, plus the
/// processor lists to run over each.
#[derive(Debug)]
pub struct FossilConfig {
    fossil_processors: Vec<Processor>,
    fossil_structures: Vec<Arc<StructureTemplate>>,
    overlay_processors: Vec<Processor>,
    overlay_structures: Vec<Arc<StructureTemplate>>,
    max_empty_corners: i32,
}

/// A configured feature.
#[derive(Debug)]
pub enum Configured {
    /// `simple_block`.
    SimpleBlock {
        provider: StateProvider,
        schedule_tick: bool,
    },
    /// `random_patch`, `flower`, `no_bonemeal_flower`.
    RandomPatch {
        tries: i32,
        xz: i32,
        y: i32,
        feature: Arc<Placed>,
    },
    /// `ore`.
    Ore {
        targets: Vec<OreTarget>,
        size: i32,
        discard: f32,
    },
    /// `scattered_ore`.
    ScatteredOre {
        targets: Vec<OreTarget>,
        size: i32,
        discard: f32,
    },
    /// `random_selector`.
    RandomSelector {
        features: Vec<(Arc<Placed>, f32)>,
        default: Arc<Placed>,
    },
    /// `simple_random_selector`.
    SimpleRandomSelector(Vec<Arc<Placed>>),
    /// `random_boolean_selector`.
    RandomBoolean(Arc<Placed>, Arc<Placed>),
    /// `block_column`.
    BlockColumn {
        layers: Vec<(IntProvider, StateProvider)>,
        dir: Dir,
        allowed: BlockPredicate,
        prioritize_tip: bool,
    },
    /// `seagrass`.
    Seagrass(f32),
    /// `kelp`.
    Kelp,
    /// `disk`.
    Disk {
        fallback: StateProvider,
        rules: Vec<(BlockPredicate, StateProvider)>,
        target: BlockPredicate,
        radius: IntProvider,
        half_height: i32,
    },
    /// `freeze_top_layer`.
    FreezeTopLayer,
    /// `vines`.
    Vines,
    /// `sea_pickle`.
    SeaPickle(IntProvider),
    /// `spring_feature`.
    Spring {
        state: BlockStateId,
        requires_below: bool,
        rock_count: i32,
        hole_count: i32,
        valid: BlockSet,
    },
    /// `lake`.
    Lake {
        fluid: StateProvider,
        barrier: StateProvider,
    },
    /// `bamboo`.
    Bamboo(f32),
    /// `forest_rock` (`BlockBlobFeature`).
    BlockBlob(BlockStateId),
    /// `ice_spike`.
    IceSpike,
    /// `blue_ice`.
    BlueIce,
    /// `underwater_magma`.
    UnderwaterMagma {
        range: i32,
        radius: i32,
        probability: f32,
    },
    /// `multiface_growth` (glow lichen), including the `MultifaceSpreader`.
    MultifaceGrowth {
        block: BlockStateId,
        search_range: i32,
        floor: bool,
        ceiling: bool,
        wall: bool,
        spread: f32,
        on: BlockSet,
    },
    /// `huge_red_mushroom` / `huge_brown_mushroom`.
    HugeMushroom {
        red: bool,
        cap: StateProvider,
        stem: StateProvider,
        radius: i32,
    },
    /// `tree`.
    Tree(Box<TreeConfig>),
    /// `fallen_tree`.
    FallenTree(Box<FallenTree>),
    /// `vegetation_patch` / `waterlogged_vegetation_patch`.
    VegetationPatch(Box<super::extra::VegetationPatch>),
    /// `coral_tree` / `coral_claw` / `coral_mushroom`.
    Coral(super::extra::Coral, Arc<super::extra::CoralBlocks>),
    /// `iceberg`.
    Iceberg(BlockStateId),
    /// `desert_well`.
    DesertWell,
    /// `monster_room` (dungeons).
    MonsterRoom,
    /// `geode` (amethyst).
    Geode(Box<GeodeConfig>),
    /// `fossil`.
    Fossil(Box<FossilConfig>),
    /// `dripstone_cluster`.
    DripstoneCluster(Box<super::dripstone::DripstoneCluster>),
    /// `large_dripstone`.
    LargeDripstone(Box<super::dripstone::LargeDripstone>),
    /// `pointed_dripstone`.
    PointedDripstone(Box<super::dripstone::PointedDripstone>),
    /// A type this port does not place.
    Unsupported(String),
}

fn get<'j>(j: &'j Json, k: &str) -> Result<&'j Json, BuildError> {
    j.get(k)
        .ok_or_else(|| BuildError::new(format!("missing `{k}`")))
}

fn state_of(j: &Json, k: &str) -> Result<BlockStateId, BuildError> {
    parse_block_state(get(j, k)?).ok_or_else(|| BuildError::new(format!("bad block state `{k}`")))
}

fn targets(l: &Loader, c: &Json) -> Result<Vec<OreTarget>, BuildError> {
    let mut out = Vec::new();
    for t in c.get("targets").and_then(Json::as_arr).unwrap_or(&[]) {
        if let Some(state) = t.get("state").and_then(parse_block_state) {
            out.push(OreTarget {
                test: RuleTest::parse(get(t, "target")?, l.tags)?,
                state,
            });
        }
    }
    Ok(out)
}

/// Parse a configured feature.
pub(crate) fn parse(l: &mut Loader, j: &Json) -> Result<Configured, BuildError> {
    let t = j
        .str_of("type")
        .unwrap_or("")
        .trim_start_matches("minecraft:")
        .to_string();
    let empty = Json::Obj(Default::default());
    let c = j.get("config").unwrap_or(&empty);
    let sp = |l: &Loader, k: &str| -> Result<StateProvider, BuildError> {
        StateProvider::parse(get(c, k)?, &l.noises)
    };
    Ok(match t.as_str() {
        "simple_block" => Configured::SimpleBlock {
            provider: sp(l, "to_place")?,
            schedule_tick: c.bool_or("schedule_tick", false),
        },
        "random_patch" | "flower" | "no_bonemeal_flower" => Configured::RandomPatch {
            tries: c.i32_or("tries", 128),
            xz: c.i32_or("xz_spread", 7),
            y: c.i32_or("y_spread", 3),
            feature: l.placed(get(c, "feature")?)?,
        },
        "ore" => Configured::Ore {
            targets: targets(l, c)?,
            size: c.i32_or("size", 0),
            discard: c.f64_or("discard_chance_on_air_exposure", 0.0) as f32,
        },
        "scattered_ore" => Configured::ScatteredOre {
            targets: targets(l, c)?,
            size: c.i32_or("size", 0),
            discard: c.f64_or("discard_chance_on_air_exposure", 0.0) as f32,
        },
        "random_selector" => {
            let mut features = Vec::new();
            for f in c.get("features").and_then(Json::as_arr).unwrap_or(&[]) {
                features.push((
                    l.placed(get(f, "feature")?)?,
                    f.f64_or("chance", 0.0) as f32,
                ));
            }
            Configured::RandomSelector {
                features,
                default: l.placed(get(c, "default")?)?,
            }
        }
        "simple_random_selector" => {
            let mut v = Vec::new();
            match c.get("features") {
                Some(Json::Arr(a)) => {
                    for f in a {
                        v.push(l.placed(f)?);
                    }
                }
                Some(other) => v.push(l.placed(other)?),
                None => {}
            }
            Configured::SimpleRandomSelector(v)
        }
        "random_boolean_selector" => Configured::RandomBoolean(
            l.placed(get(c, "feature_true")?)?,
            l.placed(get(c, "feature_false")?)?,
        ),
        "block_column" => {
            let mut layers = Vec::new();
            for layer in c.get("layers").and_then(Json::as_arr).unwrap_or(&[]) {
                layers.push((
                    IntProvider::parse(get(layer, "height")?)?,
                    StateProvider::parse(get(layer, "provider")?, &l.noises)?,
                ));
            }
            Configured::BlockColumn {
                layers,
                dir: Dir::parse(c.str_of("direction").unwrap_or("up")).unwrap_or(Dir::Up),
                allowed: l.predicate(get(c, "allowed_placement")?)?,
                prioritize_tip: c.bool_or("prioritize_tip", false),
            }
        }
        "seagrass" => Configured::Seagrass(c.f64_or("probability", 0.0) as f32),
        "kelp" => Configured::Kelp,
        "disk" => {
            let p = get(c, "state_provider")?;
            let mut rules = Vec::new();
            for r in p.get("rules").and_then(Json::as_arr).unwrap_or(&[]) {
                rules.push((
                    l.predicate(get(r, "if_true")?)?,
                    StateProvider::parse(get(r, "then")?, &l.noises)?,
                ));
            }
            Configured::Disk {
                fallback: StateProvider::parse(get(p, "fallback")?, &l.noises)?,
                rules,
                target: l.predicate(get(c, "target")?)?,
                radius: IntProvider::parse(get(c, "radius")?)?,
                half_height: c.i32_or("half_height", 0),
            }
        }
        "freeze_top_layer" => Configured::FreezeTopLayer,
        "vines" => Configured::Vines,
        "sea_pickle" => Configured::SeaPickle(IntProvider::parse(get(c, "count")?)?),
        "spring_feature" => {
            let st = get(c, "state")?;
            let fluid = st.str_of("Name").unwrap_or("minecraft:water");
            Configured::Spring {
                state: blockinfo::parse_state(&format!("{fluid}[level=0]"))
                    .ok_or_else(|| BuildError::new("spring: bad fluid"))?,
                requires_below: c.bool_or("requires_block_below", true),
                rock_count: c.i32_or("rock_count", 4),
                hole_count: c.i32_or("hole_count", 1),
                valid: l
                    .tags
                    .holder_set(c.get("valid_blocks").unwrap_or(&Json::Null)),
            }
        }
        "lake" => Configured::Lake {
            fluid: sp(l, "fluid")?,
            barrier: sp(l, "barrier")?,
        },
        "bamboo" => Configured::Bamboo(c.f64_or("probability", 0.0) as f32),
        "forest_rock" => Configured::BlockBlob(state_of(c, "state")?),
        "ice_spike" => Configured::IceSpike,
        "blue_ice" => Configured::BlueIce,
        "underwater_magma" => Configured::UnderwaterMagma {
            range: c.i32_or("floor_search_range", 5),
            radius: c.i32_or("placement_radius_around_floor", 1),
            probability: c.f64_or("placement_probability_per_valid_position", 0.5) as f32,
        },
        "multiface_growth" => Configured::MultifaceGrowth {
            block: blockinfo::parse_state(c.str_of("block").unwrap_or("minecraft:glow_lichen"))
                .ok_or_else(|| BuildError::new("multiface: bad block"))?,
            search_range: c.i32_or("search_range", 10),
            floor: c.bool_or("can_place_on_floor", false),
            ceiling: c.bool_or("can_place_on_ceiling", false),
            wall: c.bool_or("can_place_on_wall", false),
            spread: c.f64_or("chance_of_spreading", 0.5) as f32,
            on: l
                .tags
                .holder_set(c.get("can_be_placed_on").unwrap_or(&Json::Null)),
        },
        "huge_red_mushroom" | "huge_brown_mushroom" => Configured::HugeMushroom {
            red: t == "huge_red_mushroom",
            cap: sp(l, "cap_provider")?,
            stem: sp(l, "stem_provider")?,
            radius: c.i32_or("foliage_radius", 2),
        },
        "tree" => Configured::Tree(Box::new(tree::parse_tree(l, c)?)),
        "fallen_tree" => Configured::FallenTree(Box::new(tree::parse_fallen(l, c)?)),
        "vegetation_patch" | "waterlogged_vegetation_patch" => {
            Configured::VegetationPatch(Box::new(super::extra::VegetationPatch {
                waterlogged: t == "waterlogged_vegetation_patch",
                replaceable: l
                    .tags
                    .holder_set(c.get("replaceable").unwrap_or(&Json::Null)),
                ground: sp(l, "ground_state")?,
                vegetation: l.placed(get(c, "vegetation_feature")?)?,
                ceiling: c.str_of("surface") == Some("ceiling"),
                depth: IntProvider::parse(get(c, "depth")?)?,
                extra_bottom: c.f64_or("extra_bottom_block_chance", 0.0) as f32,
                vertical_range: c.i32_or("vertical_range", 1),
                vegetation_chance: c.f64_or("vegetation_chance", 0.0) as f32,
                xz_radius: IntProvider::parse(get(c, "xz_radius")?)?,
                extra_edge: c.f64_or("extra_edge_column_chance", 0.0) as f32,
            }))
        }
        "coral_tree" | "coral_claw" | "coral_mushroom" => {
            let list = |tag: &str| -> Vec<BlockStateId> {
                l.tags
                    .block_tag_list(tag)
                    .into_iter()
                    .filter_map(|b| {
                        aether_world::registry::blocks::default_state(
                            aether_world::registry::blocks::BLOCKS[b as usize].0,
                        )
                    })
                    .collect()
            };
            let blocks = Arc::new(super::extra::CoralBlocks {
                blocks: list("minecraft:coral_blocks"),
                corals: list("minecraft:corals"),
                wall_corals: list("minecraft:wall_corals"),
                corals_set: l.tags.block_tag("minecraft:corals"),
            });
            let kind = match t.as_str() {
                "coral_tree" => super::extra::Coral::Tree,
                "coral_claw" => super::extra::Coral::Claw,
                _ => super::extra::Coral::Mushroom,
            };
            Configured::Coral(kind, blocks)
        }
        "iceberg" => Configured::Iceberg(state_of(c, "state")?),
        "desert_well" => Configured::DesertWell,
        "monster_room" => Configured::MonsterRoom,
        "geode" => {
            let blocks = get(c, "blocks")?;
            let layers = get(c, "layers")?;
            let crack = get(c, "crack")?;
            let placements = |k: &str| -> Vec<BlockStateId> {
                blocks
                    .get(k)
                    .and_then(Json::as_arr)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(parse_block_state)
                    .collect()
            };
            Configured::Geode(Box::new(GeodeConfig {
                filling: StateProvider::parse(get(blocks, "filling_provider")?, &l.noises)?,
                inner: StateProvider::parse(get(blocks, "inner_layer_provider")?, &l.noises)?,
                alternate_inner: StateProvider::parse(
                    get(blocks, "alternate_inner_layer_provider")?,
                    &l.noises,
                )?,
                middle: StateProvider::parse(get(blocks, "middle_layer_provider")?, &l.noises)?,
                outer: StateProvider::parse(get(blocks, "outer_layer_provider")?, &l.noises)?,
                inner_placements: placements("inner_placements"),
                cannot_replace: l
                    .tags
                    .holder_set(blocks.get("cannot_replace").unwrap_or(&Json::Null)),
                invalid_blocks: l
                    .tags
                    .holder_set(blocks.get("invalid_blocks").unwrap_or(&Json::Null)),
                filling_layer: layers.f64_or("filling", 1.7),
                inner_layer: layers.f64_or("inner_layer", 2.2),
                middle_layer: layers.f64_or("middle_layer", 3.2),
                outer_layer: layers.f64_or("outer_layer", 4.2),
                crack_chance: crack.f64_or("generate_crack_chance", 1.0),
                crack_base: crack.f64_or("base_crack_size", 2.0),
                crack_offset: crack.i32_or("crack_point_offset", 2),
                use_potential: c.f64_or("use_potential_placements_chance", 0.35),
                use_alternate: c.f64_or("use_alternate_layer0_chance", 0.0),
                require_alternate: c.bool_or("placements_require_layer0_alternate", true),
                outer_wall: IntProvider::parse(get(c, "outer_wall_distance")?)?,
                distribution: IntProvider::parse(get(c, "distribution_points")?)?,
                point_offset: IntProvider::parse(get(c, "point_offset")?)?,
                min_gen_offset: c.i32_or("min_gen_offset", -16),
                max_gen_offset: c.i32_or("max_gen_offset", 16),
                noise_multiplier: c.f64_or("noise_multiplier", 0.05),
                invalid_threshold: c.i32_or("invalid_blocks_threshold", 1),
            }))
        }
        "fossil" => {
            let structures = |key: &str| -> Result<Vec<Arc<StructureTemplate>>, BuildError> {
                let mut v = Vec::new();
                for id in c.get(key).and_then(Json::as_arr).unwrap_or(&[]) {
                    let id = id
                        .as_str()
                        .ok_or_else(|| BuildError::new(format!("fossil: bad `{key}` entry")))?;
                    v.push(Arc::new(structure::load_from_pack(l.pack, id)?));
                }
                Ok(v)
            };
            let processors = |key: &str| -> Result<Vec<Processor>, BuildError> {
                let id = c
                    .str_of(key)
                    .ok_or_else(|| BuildError::new(format!("fossil: no `{key}`")))?;
                let pj = l.pack.read_json("processor_list", id)?;
                structure::parse_processors(&pj, l.tags)
            };
            Configured::Fossil(Box::new(FossilConfig {
                fossil_processors: processors("fossil_processors")?,
                fossil_structures: structures("fossil_structures")?,
                overlay_processors: processors("overlay_processors")?,
                overlay_structures: structures("overlay_structures")?,
                max_empty_corners: c.i32_or("max_empty_corners_allowed", 4),
            }))
        }
        "dripstone_cluster" => {
            Configured::DripstoneCluster(Box::new(super::dripstone::DripstoneCluster::parse(c)?))
        }
        "large_dripstone" => {
            Configured::LargeDripstone(Box::new(super::dripstone::LargeDripstone::parse(c)?))
        }
        "pointed_dripstone" => {
            Configured::PointedDripstone(Box::new(super::dripstone::PointedDripstone::parse(c)?))
        }
        other => {
            l.unsupported.insert(format!("feature:{other}"));
            Configured::Unsupported(other.to_string())
        }
    })
}

/// Commonly placed states.
pub(crate) struct States {
    pub water: BlockStateId,
    pub ice: BlockStateId,
    pub snow: BlockStateId,
    pub packed_ice: BlockStateId,
    pub blue_ice: BlockStateId,
    pub snow_block: BlockStateId,
    pub magma: BlockStateId,
    pub podzol: BlockStateId,
    pub bamboo_trunk: BlockStateId,
    pub bamboo_final_large: BlockStateId,
    pub bamboo_top_large: BlockStateId,
    pub bamboo_top_small: BlockStateId,
    pub kelp: BlockStateId,
    pub kelp_plant: BlockStateId,
    pub seagrass: BlockStateId,
    pub tall_seagrass: BlockStateId,
    pub sea_pickle: BlockStateId,
    pub vine: BlockStateId,
    pub cave_air: BlockStateId,
    pub sand: BlockStateId,
    pub sandstone: BlockStateId,
    pub sandstone_slab: BlockStateId,
    pub suspicious_sand: BlockStateId,
    pub cobblestone: BlockStateId,
    pub mossy_cobblestone: BlockStateId,
    pub spawner: BlockStateId,
    pub chest: BlockStateId,
}

pub(crate) fn states() -> &'static States {
    static S: OnceLock<States> = OnceLock::new();
    S.get_or_init(|| {
        let s = |n: &str| blockinfo::parse_state(n).unwrap_or_else(|| panic!("no block state {n}"));
        States {
            water: s("minecraft:water[level=0]"),
            ice: s("minecraft:ice"),
            snow: s("minecraft:snow"),
            packed_ice: s("minecraft:packed_ice"),
            blue_ice: s("minecraft:blue_ice"),
            snow_block: s("minecraft:snow_block"),
            magma: s("minecraft:magma_block"),
            podzol: s("minecraft:podzol"),
            bamboo_trunk: s("minecraft:bamboo[age=0,leaves=none,stage=0]"),
            bamboo_final_large: s("minecraft:bamboo[age=1,leaves=large,stage=1]"),
            bamboo_top_large: s("minecraft:bamboo[age=1,leaves=large,stage=0]"),
            bamboo_top_small: s("minecraft:bamboo[age=1,leaves=small,stage=0]"),
            kelp: s("minecraft:kelp"),
            kelp_plant: s("minecraft:kelp_plant"),
            seagrass: s("minecraft:seagrass"),
            tall_seagrass: s("minecraft:tall_seagrass[half=lower]"),
            sea_pickle: s("minecraft:sea_pickle"),
            vine: s("minecraft:vine"),
            cave_air: s("minecraft:cave_air"),
            sand: s("minecraft:sand"),
            sandstone: s("minecraft:sandstone"),
            sandstone_slab: s("minecraft:sandstone_slab"),
            suspicious_sand: s("minecraft:suspicious_sand"),
            cobblestone: s("minecraft:cobblestone"),
            mossy_cobblestone: s("minecraft:mossy_cobblestone"),
            spawner: s("minecraft:spawner"),
            chest: s("minecraft:chest"),
        }
    })
}

fn is(s: BlockStateId, name: &str) -> bool {
    blockinfo::name(s) == name
}

/// `ConfiguredFeature.place`: refuses outright outside the writable region.
pub(crate) fn place_configured(ctx: &mut Ctx, f: &Configured, pos: Pos) -> bool {
    if !ctx.lv.can_write(pos) {
        return false;
    }
    match f {
        Configured::SimpleBlock { provider, .. } => simple_block(ctx, provider, pos),
        Configured::RandomPatch {
            tries,
            xz,
            y,
            feature,
        } => {
            let mut n = 0;
            let (a, b) = (xz + 1, y + 1);
            for _ in 0..*tries {
                let dx = ctx.r.next_int_bounded(a) - ctx.r.next_int_bounded(a);
                let dy = ctx.r.next_int_bounded(b) - ctx.r.next_int_bounded(b);
                let dz = ctx.r.next_int_bounded(a) - ctx.r.next_int_bounded(a);
                if place_placed(ctx, feature, pos.offset(dx, dy, dz), None) {
                    n += 1;
                }
            }
            n > 0
        }
        Configured::Ore {
            targets,
            size,
            discard,
        } => ore(ctx, targets, *size, *discard, pos),
        Configured::ScatteredOre {
            targets,
            size,
            discard,
        } => {
            let n = ctx.r.next_int_bounded(size + 1);
            for i in 0..n {
                let d = i.min(7);
                let mut axis =
                    || ((ctx.r.next_float() - ctx.r.next_float()) * d as f32).round() as i32;
                let (dx, dy, dz) = (axis(), axis(), axis());
                let p = pos.offset(dx, dy, dz);
                let s = ctx.lv.get(p);
                for t in targets {
                    if can_place_ore(ctx, s, t, *discard, p) {
                        ctx.lv.set(p, t.state);
                        break;
                    }
                }
            }
            true
        }
        Configured::RandomSelector { features, default } => {
            for (p, chance) in features {
                if ctx.r.next_float() < *chance {
                    return place_placed(ctx, p, pos, None);
                }
            }
            place_placed(ctx, default, pos, None)
        }
        Configured::SimpleRandomSelector(list) => {
            if list.is_empty() {
                return false;
            }
            let i = ctx.r.next_int_bounded(list.len() as i32) as usize;
            place_placed(ctx, &list[i], pos, None)
        }
        Configured::RandomBoolean(a, b) => {
            let which = ctx.r.next_bool();
            place_placed(ctx, if which { a } else { b }, pos, None)
        }
        Configured::BlockColumn {
            layers,
            dir,
            allowed,
            prioritize_tip,
        } => block_column(ctx, layers, *dir, allowed, *prioritize_tip, pos),
        Configured::Seagrass(prob) => seagrass(ctx, *prob, pos),
        Configured::Kelp => kelp(ctx, pos),
        Configured::Disk {
            fallback,
            rules,
            target,
            radius,
            half_height,
        } => disk(ctx, fallback, rules, target, radius, *half_height, pos),
        Configured::FreezeTopLayer => freeze_top_layer(ctx, pos),
        Configured::Vines => {
            if !ctx.lv.is_air(pos) {
                return false;
            }
            for d in Dir::ALL {
                if d != Dir::Down && sturdy(ctx.lv.get(pos.rel(d))) {
                    let v = blockinfo::with_prop(states().vine, d.name(), "true");
                    ctx.lv.set(pos, v);
                    return true;
                }
            }
            false
        }
        Configured::SeaPickle(count) => {
            let n = count.sample(ctx.r);
            let mut placed = 0;
            for _ in 0..n {
                let dx = ctx.r.next_int_bounded(8) - ctx.r.next_int_bounded(8);
                let dz = ctx.r.next_int_bounded(8) - ctx.r.next_int_bounded(8);
                let y = ctx.lv.height(Heightmap::OceanFloor, pos.x + dx, pos.z + dz);
                let p = Pos::new(pos.x + dx, y, pos.z + dz);
                let pickles = ctx.r.next_int_bounded(4) + 1;
                let st = blockinfo::with_prop(states().sea_pickle, "pickles", &pickles.to_string());
                if is(ctx.lv.get(p), "minecraft:water") && can_survive(ctx.lv, ctx.tags(), st, p) {
                    ctx.lv.set(p, st);
                    placed += 1;
                }
            }
            placed > 0
        }
        Configured::Spring {
            state,
            requires_below,
            rock_count,
            hole_count,
            valid,
        } => {
            let lv = &ctx.lv;
            if !valid.contains(lv.get(pos.above(1))) {
                return false;
            }
            if *requires_below && !valid.contains(lv.get(pos.below(1))) {
                return false;
            }
            let here = lv.get(pos);
            if !blockinfo::is_air(here) && !valid.contains(here) {
                return false;
            }
            let around = [
                pos.rel(Dir::West),
                pos.rel(Dir::East),
                pos.rel(Dir::North),
                pos.rel(Dir::South),
                pos.below(1),
            ];
            let rocks = around
                .iter()
                .filter(|p| valid.contains(lv.get(**p)))
                .count() as i32;
            let holes = around.iter().filter(|p| lv.is_air(**p)).count() as i32;
            if rocks == *rock_count && holes == *hole_count {
                ctx.lv.set(pos, *state);
                true
            } else {
                false
            }
        }
        Configured::Lake { fluid, barrier } => lake(ctx, fluid, barrier, pos),
        Configured::Bamboo(prob) => bamboo(ctx, *prob, pos),
        Configured::BlockBlob(state) => block_blob(ctx, *state, pos),
        Configured::IceSpike => ice_spike(ctx, pos),
        Configured::BlueIce => blue_ice(ctx, pos),
        Configured::UnderwaterMagma {
            range,
            radius,
            probability,
        } => underwater_magma(ctx, *range, *radius, *probability, pos),
        Configured::MultifaceGrowth {
            block,
            search_range,
            floor,
            ceiling,
            wall,
            spread,
            on,
        } => multiface(
            ctx,
            *block,
            *search_range,
            *floor,
            *ceiling,
            *wall,
            *spread,
            on,
            pos,
        ),
        Configured::HugeMushroom {
            red,
            cap,
            stem,
            radius,
        } => huge_mushroom(ctx, *red, cap, stem, *radius, pos),
        Configured::Tree(cfg) => tree::place_tree(ctx, cfg, pos),
        Configured::FallenTree(cfg) => tree::place_fallen(ctx, cfg, pos),
        Configured::VegetationPatch(cfg) => super::extra::vegetation_patch(ctx, cfg, pos),
        Configured::Coral(kind, blocks) => super::extra::coral(ctx, *kind, blocks, pos),
        Configured::Iceberg(state) => super::extra::iceberg(ctx, *state, pos),
        Configured::DesertWell => desert_well(ctx, pos),
        Configured::MonsterRoom => monster_room(ctx, pos),
        Configured::Geode(cfg) => geode(ctx, cfg, pos),
        Configured::Fossil(cfg) => fossil(ctx, cfg, pos),
        Configured::DripstoneCluster(cfg) => super::dripstone::dripstone_cluster(ctx, cfg, pos),
        Configured::LargeDripstone(cfg) => super::dripstone::large_dripstone(ctx, cfg, pos),
        Configured::PointedDripstone(cfg) => super::dripstone::pointed_dripstone(ctx, cfg, pos),
        Configured::Unsupported(name) => {
            ctx.d.note_unsupported(&format!("feature:{name}"));
            false
        }
    }
}

fn is_double_plant(s: BlockStateId) -> bool {
    matches!(
        blockinfo::name(s),
        "minecraft:tall_grass"
            | "minecraft:large_fern"
            | "minecraft:sunflower"
            | "minecraft:lilac"
            | "minecraft:rose_bush"
            | "minecraft:peony"
            | "minecraft:pitcher_plant"
            | "minecraft:tall_seagrass"
            | "minecraft:small_dripleaf"
            | "minecraft:tall_dry_grass"
    ) && prop(s, "half").is_some()
}

/// `DoublePlantBlock.copyWaterloggedFrom`.
fn waterlog_from(ctx: &Ctx, p: Pos, s: BlockStateId) -> BlockStateId {
    if prop(s, "waterlogged").is_some() {
        let w = is_water_source(ctx.lv.get(p)) || blockinfo::has_water(ctx.lv.get(p));
        blockinfo::with_prop(s, "waterlogged", if w { "true" } else { "false" })
    } else {
        s
    }
}

fn simple_block(ctx: &mut Ctx, provider: &StateProvider, pos: Pos) -> bool {
    let s = provider.get(ctx.r, pos);
    if !can_survive(ctx.lv, ctx.tags(), s, pos) {
        return false;
    }
    if is_double_plant(s) {
        if !ctx.lv.is_air(pos.above(1)) {
            return false;
        }
        let lower = waterlog_from(ctx, pos, blockinfo::with_prop(s, "half", "lower"));
        let upper = waterlog_from(ctx, pos.above(1), blockinfo::with_prop(s, "half", "upper"));
        ctx.lv.set(pos, lower);
        ctx.lv.set(pos.above(1), upper);
    } else if is(s, "minecraft:pale_moss_carpet") {
        // MossyCarpetBlock.placeAt: the carpet plus, half the time, a
        // hanging part above it; drawn from the region's own random.
        ctx.lv.set(pos, s);
    } else {
        ctx.lv.set(pos, s);
    }
    true
}

fn can_place_ore(ctx: &mut Ctx, s: BlockStateId, t: &OreTarget, discard: f32, p: Pos) -> bool {
    if !t.test.test(s, ctx.r) {
        return false;
    }
    let skip_air_check = if discard <= 0.0 {
        true
    } else if discard >= 1.0 {
        false
    } else {
        ctx.r.next_float() >= discard
    };
    if skip_air_check {
        return true;
    }
    !Dir::ALL.iter().any(|d| ctx.lv.is_air(p.rel(*d)))
}

/// `OreFeature`.
fn ore(ctx: &mut Ctx, targets: &[OreTarget], size: i32, discard: f32, pos: Pos) -> bool {
    use std::f64::consts::PI;
    let angle = ctx.r.next_float() * PI as f32;
    let f = size as f32 / 8.0;
    let i = ((size as f32 / 16.0 * 2.0 + 1.0) / 2.0).ceil() as i32;
    let x0 = pos.x as f64 + (angle as f64).sin() * f as f64;
    let x1 = pos.x as f64 - (angle as f64).sin() * f as f64;
    let z0 = pos.z as f64 + (angle as f64).cos() * f as f64;
    let z1 = pos.z as f64 - (angle as f64).cos() * f as f64;
    let y0 = (pos.y + ctx.r.next_int_bounded(3) - 2) as f64;
    let y1 = (pos.y + ctx.r.next_int_bounded(3) - 2) as f64;
    let fc = f.ceil() as i32;
    let min_x = pos.x - fc - i;
    let min_y = pos.y - 2 - i;
    let min_z = pos.z - fc - i;
    let w = 2 * (fc + i);
    let h = 2 * (2 + i);
    for x in min_x..=min_x + w {
        for z in min_z..=min_z + w {
            if min_y <= ctx.lv.height(Heightmap::OceanFloorWg, x, z) {
                return ore_place(
                    ctx,
                    targets,
                    size,
                    discard,
                    [x0, x1, z0, z1, y0, y1],
                    min_x,
                    min_y,
                    min_z,
                    w,
                    h,
                );
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn ore_place(
    ctx: &mut Ctx,
    targets: &[OreTarget],
    size: i32,
    discard: f32,
    v: [f64; 6],
    min_x: i32,
    min_y: i32,
    min_z: i32,
    w: i32,
    h: i32,
) -> bool {
    let [x0, x1, z0, z1, y0, y1] = v;
    let n = size as usize;
    let mut balls = vec![0f64; n * 4];
    for k in 0..n {
        let t = k as f32 / size as f32;
        let x = x0 + t as f64 * (x1 - x0);
        let y = y0 + t as f64 * (y1 - y0);
        let z = z0 + t as f64 * (z1 - z0);
        let s = ctx.r.next_double() * size as f64 / 16.0;
        let r = ((super::super::mth::sin((std::f32::consts::PI * t) as f64) + 1.0) as f64 * s
            + 1.0)
            / 2.0;
        balls[k * 4] = x;
        balls[k * 4 + 1] = y;
        balls[k * 4 + 2] = z;
        balls[k * 4 + 3] = r;
    }
    for a in 0..n.saturating_sub(1) {
        if balls[a * 4 + 3] <= 0.0 {
            continue;
        }
        for b in a + 1..n {
            if balls[b * 4 + 3] <= 0.0 {
                continue;
            }
            let dx = balls[a * 4] - balls[b * 4];
            let dy = balls[a * 4 + 1] - balls[b * 4 + 1];
            let dz = balls[a * 4 + 2] - balls[b * 4 + 2];
            let dr = balls[a * 4 + 3] - balls[b * 4 + 3];
            if dr * dr > dx * dx + dy * dy + dz * dz {
                if dr > 0.0 {
                    balls[b * 4 + 3] = -1.0;
                } else {
                    balls[a * 4 + 3] = -1.0;
                }
            }
        }
    }
    let mut done = vec![false; (w * h * w).max(0) as usize + 1];
    let mut placed = 0;
    for k in 0..n {
        let r = balls[k * 4 + 3];
        if r < 0.0 {
            continue;
        }
        let (bx, by, bz) = (balls[k * 4], balls[k * 4 + 1], balls[k * 4 + 2]);
        let xa = ((bx - r).floor() as i32).max(min_x);
        let ya = ((by - r).floor() as i32).max(min_y);
        let za = ((bz - r).floor() as i32).max(min_z);
        let xb = ((bx + r).floor() as i32).max(xa);
        let yb = ((by + r).floor() as i32).max(ya);
        let zb = ((bz + r).floor() as i32).max(za);
        for x in xa..=xb {
            let dx = (x as f64 + 0.5 - bx) / r;
            if dx * dx >= 1.0 {
                continue;
            }
            for y in ya..=yb {
                let dy = (y as f64 + 0.5 - by) / r;
                if dx * dx + dy * dy >= 1.0 {
                    continue;
                }
                for z in za..=zb {
                    let dz = (z as f64 + 0.5 - bz) / r;
                    if dx * dx + dy * dy + dz * dz >= 1.0 || ctx.lv.outside_height(y) {
                        continue;
                    }
                    let idx = (x - min_x + (y - min_y) * w + (z - min_z) * w * h) as usize;
                    if idx >= done.len() || done[idx] {
                        continue;
                    }
                    done[idx] = true;
                    let p = Pos::new(x, y, z);
                    if !ctx.lv.can_write(p) {
                        continue;
                    }
                    let s = ctx.lv.get(p);
                    for t in targets {
                        if can_place_ore(ctx, s, t, discard, p) {
                            ctx.lv.set_raw(p, t.state);
                            placed += 1;
                            break;
                        }
                    }
                }
            }
        }
    }
    placed > 0
}

fn block_column(
    ctx: &mut Ctx,
    layers: &[(IntProvider, StateProvider)],
    dir: Dir,
    allowed: &BlockPredicate,
    prioritize_tip: bool,
    pos: Pos,
) -> bool {
    let mut heights: Vec<i32> = layers.iter().map(|(h, _)| h.sample(ctx.r)).collect();
    let total: i32 = heights.iter().sum();
    if total == 0 {
        return false;
    }
    let mut probe = pos.rel(dir);
    for i in 0..total {
        if !allowed.test(ctx.lv, ctx.tags(), probe) {
            // truncate
            let mut left = total - i;
            let order: Vec<usize> = if prioritize_tip {
                (0..heights.len()).collect()
            } else {
                (0..heights.len()).rev().collect()
            };
            for k in order {
                if left <= 0 {
                    break;
                }
                let take = heights[k].min(left);
                left -= take;
                heights[k] -= take;
            }
            break;
        }
        probe = probe.rel(dir);
    }
    let mut p = pos;
    for (k, (_, provider)) in layers.iter().enumerate() {
        for _ in 0..heights[k] {
            let s = provider.get(ctx.r, p);
            ctx.lv.set(p, s);
            p = p.rel(dir);
        }
    }
    true
}

fn seagrass(ctx: &mut Ctx, prob: f32, pos: Pos) -> bool {
    let dx = ctx.r.next_int_bounded(8) - ctx.r.next_int_bounded(8);
    let dz = ctx.r.next_int_bounded(8) - ctx.r.next_int_bounded(8);
    let y = ctx.lv.height(Heightmap::OceanFloor, pos.x + dx, pos.z + dz);
    let p = Pos::new(pos.x + dx, y, pos.z + dz);
    if !is(ctx.lv.get(p), "minecraft:water") {
        return false;
    }
    let tall = ctx.r.next_double() < prob as f64;
    let st = if tall {
        states().tall_seagrass
    } else {
        states().seagrass
    };
    if !can_survive(ctx.lv, ctx.tags(), st, p) {
        return false;
    }
    if tall {
        let up = p.above(1);
        if is(ctx.lv.get(up), "minecraft:water") {
            ctx.lv.set(p, st);
            ctx.lv.set(up, blockinfo::with_prop(st, "half", "upper"));
        }
    } else {
        ctx.lv.set(p, st);
    }
    true
}

fn kelp(ctx: &mut Ctx, pos: Pos) -> bool {
    let y = ctx.lv.height(Heightmap::OceanFloor, pos.x, pos.z);
    let mut p = Pos::new(pos.x, y, pos.z);
    let mut placed = 0;
    if !is(ctx.lv.get(p), "minecraft:water") {
        return false;
    }
    let st = states();
    let len = 1 + ctx.r.next_int_bounded(10);
    for i in 0..=len {
        if is(ctx.lv.get(p), "minecraft:water")
            && is(ctx.lv.get(p.above(1)), "minecraft:water")
            && can_survive(ctx.lv, ctx.tags(), st.kelp_plant, p)
        {
            if i == len {
                let age = ctx.r.next_int_bounded(4) + 20;
                ctx.lv
                    .set(p, blockinfo::with_prop(st.kelp, "age", &age.to_string()));
                placed += 1;
            } else {
                ctx.lv.set(p, st.kelp_plant);
            }
        } else if i > 0 {
            let b = p.below(1);
            if can_survive(ctx.lv, ctx.tags(), st.kelp, b)
                && !is(ctx.lv.get(b.below(1)), "minecraft:kelp")
            {
                let age = ctx.r.next_int_bounded(4) + 20;
                ctx.lv
                    .set(b, blockinfo::with_prop(st.kelp, "age", &age.to_string()));
                placed += 1;
            }
            break;
        }
        p = p.above(1);
    }
    placed > 0
}

#[allow(clippy::too_many_arguments)]
fn disk(
    ctx: &mut Ctx,
    fallback: &StateProvider,
    rules: &[(BlockPredicate, StateProvider)],
    target: &BlockPredicate,
    radius: &IntProvider,
    half: i32,
    pos: Pos,
) -> bool {
    let top = pos.y + half;
    let bottom = pos.y - half - 1;
    let r = radius.sample(ctx.r);
    let mut any = false;
    // betweenClosed: x fastest, then z (y is fixed).
    for dz in -r..=r {
        for dx in -r..=r {
            if dx * dx + dz * dz > r * r {
                continue;
            }
            let mut y = top;
            while y > bottom {
                let p = Pos::new(pos.x + dx, y, pos.z + dz);
                if target.test(ctx.lv, ctx.tags(), p) {
                    let mut chosen = None;
                    for (pred, prov) in rules {
                        if pred.test(ctx.lv, ctx.tags(), p) {
                            chosen = Some(prov.get(ctx.r, p));
                            break;
                        }
                    }
                    let s = chosen.unwrap_or_else(|| fallback.get(ctx.r, p));
                    ctx.lv.set(p, s);
                    any = true;
                }
                y -= 1;
            }
        }
    }
    any
}

/// `Biome.shouldFreeze(level, pos, false)`.
fn should_freeze(ctx: &Ctx, biome: super::super::biome::BiomeId, p: Pos) -> bool {
    let core = ctx.lv.core;
    if !core
        .biomes
        .cold_enough_to_snow(biome, p.x, p.y, p.z, core.sea_level)
    {
        return false;
    }
    if ctx.lv.outside_height(p.y) {
        return false;
    }
    let s = ctx.lv.get(p);
    is(s, "minecraft:water") && is_water_source(s)
}

/// `Biome.shouldSnow`.
fn should_snow(ctx: &Ctx, biome: super::super::biome::BiomeId, p: Pos) -> bool {
    let core = ctx.lv.core;
    let info = core.biomes.get(biome);
    if !info.has_precipitation
        || !core
            .biomes
            .cold_enough_to_snow(biome, p.x, p.y, p.z, core.sea_level)
    {
        return false;
    }
    if ctx.lv.outside_height(p.y) {
        return false;
    }
    let s = ctx.lv.get(p);
    (blockinfo::is_air(s) || is(s, "minecraft:snow"))
        && can_survive(ctx.lv, ctx.tags(), states().snow, p)
}

fn freeze_top_layer(ctx: &mut Ctx, pos: Pos) -> bool {
    for dx in 0..16 {
        for dz in 0..16 {
            let x = pos.x + dx;
            let z = pos.z + dz;
            let y = ctx.lv.height(Heightmap::MotionBlocking, x, z);
            let top = Pos::new(x, y, z);
            let below = top.below(1);
            let biome = ctx.lv.biome(top);
            if should_freeze(ctx, biome, below) {
                ctx.lv.set(below, states().ice);
            }
            if should_snow(ctx, biome, top) {
                ctx.lv.set(top, states().snow);
                let b = ctx.lv.get(below);
                if prop(b, "snowy").is_some() {
                    ctx.lv.set(below, blockinfo::with_prop(b, "snowy", "true"));
                }
            }
        }
    }
    true
}

fn lake(ctx: &mut Ctx, fluid_p: &StateProvider, barrier_p: &StateProvider, pos: Pos) -> bool {
    if pos.y <= ctx.lv.min_y() + 4 {
        return false;
    }
    let base = pos.below(4);
    let mut grid = vec![false; 2048];
    let n = ctx.r.next_int_bounded(4) + 4;
    for _ in 0..n {
        let sx = ctx.r.next_double() * 6.0 + 3.0;
        let sy = ctx.r.next_double() * 4.0 + 2.0;
        let sz = ctx.r.next_double() * 6.0 + 3.0;
        let cx = ctx.r.next_double() * (16.0 - sx - 2.0) + 1.0 + sx / 2.0;
        let cy = ctx.r.next_double() * (8.0 - sy - 4.0) + 2.0 + sy / 2.0;
        let cz = ctx.r.next_double() * (16.0 - sz - 2.0) + 1.0 + sz / 2.0;
        for x in 1..15 {
            for z in 1..15 {
                for y in 1..7 {
                    let dx = (x as f64 - cx) / (sx / 2.0);
                    let dy = (y as f64 - cy) / (sy / 2.0);
                    let dz = (z as f64 - cz) / (sz / 2.0);
                    if dx * dx + dy * dy + dz * dz < 1.0 {
                        grid[(x * 16 + z) * 8 + y] = true;
                    }
                }
            }
        }
    }
    let at = |x: usize, z: usize, y: usize| grid[(x * 16 + z) * 8 + y];
    let edge = |x: usize, z: usize, y: usize| {
        !at(x, z, y)
            && (x < 15 && at(x + 1, z, y)
                || x > 0 && at(x - 1, z, y)
                || z < 15 && at(x, z + 1, y)
                || z > 0 && at(x, z - 1, y)
                || y < 7 && at(x, z, y + 1)
                || y > 0 && at(x, z, y - 1))
    };
    let fluid = fluid_p.get(ctx.r, base);
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..8 {
                if edge(x, z, y) {
                    let s = ctx.lv.get(base.offset(x as i32, y as i32, z as i32));
                    if y >= 4 && matches!(blockinfo::name(s), "minecraft:water" | "minecraft:lava")
                    {
                        return false;
                    }
                    if y < 4 && !legacy_solid(s) && s != fluid {
                        return false;
                    }
                }
            }
        }
    }
    let cannot = &ctx.d.tags.features_cannot_replace;
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..8 {
                if at(x, z, y) {
                    let p = base.offset(x as i32, y as i32, z as i32);
                    if !cannot.contains(ctx.lv.get(p)) {
                        ctx.lv
                            .set(p, if y >= 4 { states().cave_air } else { fluid });
                    }
                }
            }
        }
    }
    let barrier = barrier_p.get(ctx.r, base);
    if !blockinfo::is_air(barrier) {
        let stone_cannot = ctx
            .d
            .block_tags
            .block_tag("minecraft:lava_pool_stone_cannot_replace");
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..8 {
                    if edge(x, z, y) && (y < 4 || ctx.r.next_int_bounded(2) != 0) {
                        let p = base.offset(x as i32, y as i32, z as i32);
                        let s = ctx.lv.get(p);
                        if legacy_solid(s) && !stone_cannot.contains(s) {
                            ctx.lv.set(p, barrier);
                        }
                    }
                }
            }
        }
    }
    if blockinfo::has_water(fluid) {
        for x in 0..16 {
            for z in 0..16 {
                let p = base.offset(x, 4, z);
                let b = ctx.lv.biome(p);
                if should_freeze(ctx, b, p) && !cannot.contains(ctx.lv.get(p)) {
                    ctx.lv.set(p, states().ice);
                }
            }
        }
    }
    true
}

fn bamboo(ctx: &mut Ctx, prob: f32, pos: Pos) -> bool {
    let st = states();
    if !ctx.lv.is_air(pos) {
        return false;
    }
    if can_survive(ctx.lv, ctx.tags(), st.bamboo_trunk, pos) {
        let height = ctx.r.next_int_bounded(12) + 5;
        if ctx.r.next_float() < prob {
            let r = ctx.r.next_int_bounded(4) + 1;
            for x in pos.x - r..=pos.x + r {
                for z in pos.z - r..=pos.z + r {
                    let (dx, dz) = (x - pos.x, z - pos.z);
                    if dx * dx + dz * dz <= r * r {
                        let y = ctx.lv.height(Heightmap::WorldSurface, x, z) - 1;
                        let p = Pos::new(x, y, z);
                        if ctx.d.tags.dirt.contains(ctx.lv.get(p)) {
                            ctx.lv.set(p, st.podzol);
                        }
                    }
                }
            }
        }
        let mut p = pos;
        let mut i = 0;
        while i < height && ctx.lv.is_air(p) {
            ctx.lv.set(p, st.bamboo_trunk);
            p = p.above(1);
            i += 1;
        }
        if p.y - pos.y >= 3 {
            ctx.lv.set(p, st.bamboo_final_large);
            ctx.lv.set(p.below(1), st.bamboo_top_large);
            ctx.lv.set(p.below(2), st.bamboo_top_small);
        }
    }
    true
}

fn block_blob(ctx: &mut Ctx, state: BlockStateId, pos: Pos) -> bool {
    let mut p = pos;
    while p.y > ctx.lv.min_y() + 3 {
        if !ctx.lv.is_air(p.below(1)) {
            let b = ctx.lv.get(p.below(1));
            if ctx.d.tags.dirt.contains(b) || ctx.d.tags.base_stone_overworld.contains(b) {
                break;
            }
        }
        p = p.below(1);
    }
    if p.y <= ctx.lv.min_y() + 3 {
        return false;
    }
    for _ in 0..3 {
        let a = ctx.r.next_int_bounded(2);
        let b = ctx.r.next_int_bounded(2);
        let c = ctx.r.next_int_bounded(2);
        let f = (a + b + c) as f32 * 0.333 + 0.5;
        for z in -c..=c {
            for y in -b..=b {
                for x in -a..=a {
                    let d = (x * x + y * y + z * z) as f64;
                    if d <= (f * f) as f64 {
                        ctx.lv.set(p.offset(x, y, z), state);
                    }
                }
            }
        }
        let dx = -1 + ctx.r.next_int_bounded(2);
        let dy = -ctx.r.next_int_bounded(2);
        let dz = -1 + ctx.r.next_int_bounded(2);
        p = p.offset(dx, dy, dz);
    }
    true
}

fn ice_spike(ctx: &mut Ctx, pos: Pos) -> bool {
    let st = states();
    let mut p = pos;
    while ctx.lv.is_air(p) && p.y > ctx.lv.min_y() + 2 {
        p = p.below(1);
    }
    if !is(ctx.lv.get(p), "minecraft:snow_block") {
        return false;
    }
    p = p.above(ctx.r.next_int_bounded(4));
    let h = ctx.r.next_int_bounded(4) + 7;
    let w = h / 4 + ctx.r.next_int_bounded(2);
    if w > 1 && ctx.r.next_int_bounded(60) == 0 {
        p = p.above(10 + ctx.r.next_int_bounded(30));
    }
    let replaceable = |ctx: &Ctx, s: BlockStateId| {
        blockinfo::is_air(s)
            || ctx.d.tags.dirt.contains(s)
            || is(s, "minecraft:snow_block")
            || is(s, "minecraft:ice")
    };
    for i in 0..h {
        let f = (1.0 - i as f32 / h as f32) * w as f32;
        let r = f.ceil() as i32;
        for dx in -r..=r {
            let fx = dx.abs() as f32 - 0.25;
            for dz in -r..=r {
                let fz = dz.abs() as f32 - 0.25;
                let inside = dx == 0 && dz == 0 || fx * fx + fz * fz <= f * f;
                if inside {
                    let edge = dx == -r || dx == r || dz == -r || dz == r;
                    if edge && ctx.r.next_float() > 0.75 {
                        continue;
                    }
                    let q = p.offset(dx, i, dz);
                    if replaceable(ctx, ctx.lv.get(q)) {
                        ctx.lv.set(q, st.packed_ice);
                    }
                    if i != 0 && r > 1 {
                        let q = p.offset(dx, -i, dz);
                        if replaceable(ctx, ctx.lv.get(q)) {
                            ctx.lv.set(q, st.packed_ice);
                        }
                    }
                }
            }
        }
    }
    let k = (w - 1).clamp(0, 1);
    for dx in -k..=k {
        for dz in -k..=k {
            let mut q = p.offset(dx, -1, dz);
            let mut left = 50;
            if dx.abs() == 1 && dz.abs() == 1 {
                left = ctx.r.next_int_bounded(5);
            }
            while q.y > 50 {
                let s = ctx.lv.get(q);
                if !blockinfo::is_air(s)
                    && !ctx.d.tags.dirt.contains(s)
                    && !is(s, "minecraft:snow_block")
                    && !is(s, "minecraft:ice")
                    && !is(s, "minecraft:packed_ice")
                {
                    break;
                }
                ctx.lv.set(q, st.packed_ice);
                q = q.below(1);
                left -= 1;
                if left <= 0 {
                    q = q.below(ctx.r.next_int_bounded(5) + 1);
                    left = ctx.r.next_int_bounded(5);
                }
            }
        }
    }
    true
}

fn blue_ice(ctx: &mut Ctx, pos: Pos) -> bool {
    let st = states();
    if pos.y > ctx.lv.sea_level() - 1 {
        return false;
    }
    if !is(ctx.lv.get(pos), "minecraft:water") && !is(ctx.lv.get(pos.below(1)), "minecraft:water") {
        return false;
    }
    if !Dir::ALL
        .iter()
        .any(|d| *d != Dir::Down && is(ctx.lv.get(pos.rel(*d)), "minecraft:packed_ice"))
    {
        return false;
    }
    ctx.lv.set(pos, st.blue_ice);
    for _ in 0..200 {
        let dy = ctx.r.next_int_bounded(5) - ctx.r.next_int_bounded(6);
        let mut range = 3;
        if dy < 2 {
            range += dy / 2;
        }
        if range >= 1 {
            let dx = ctx.r.next_int_bounded(range) - ctx.r.next_int_bounded(range);
            let dz = ctx.r.next_int_bounded(range) - ctx.r.next_int_bounded(range);
            let q = pos.offset(dx, dy, dz);
            let s = ctx.lv.get(q);
            if blockinfo::is_air(s)
                || is(s, "minecraft:water")
                || is(s, "minecraft:packed_ice")
                || is(s, "minecraft:ice")
            {
                if Dir::ALL
                    .iter()
                    .any(|d| is(ctx.lv.get(q.rel(*d)), "minecraft:blue_ice"))
                {
                    ctx.lv.set(q, st.blue_ice);
                }
            }
        }
    }
    true
}

fn underwater_magma(ctx: &mut Ctx, range: i32, radius: i32, prob: f32, pos: Pos) -> bool {
    let water = |s: BlockStateId| is(s, "minecraft:water");
    if !water(ctx.lv.get(pos)) {
        return false;
    }
    // Column.scan downward: stop at the first non-water block within range.
    let mut y = pos.y;
    let mut i = 1;
    while i < range && water(ctx.lv.get(Pos::new(pos.x, y, pos.z))) {
        y -= 1;
        i += 1;
    }
    if water(ctx.lv.get(Pos::new(pos.x, y, pos.z))) {
        return false;
    }
    let floor = Pos::new(pos.x, y, pos.z);
    let mut placed = 0;
    // betweenClosedStream over the box: x fastest, then y, then z.
    for dz in -radius..=radius {
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if ctx.r.next_float() >= prob {
                    continue;
                }
                let q = floor.offset(dx, dy, dz);
                let s = ctx.lv.get(q);
                if water(s) || blockinfo::is_air(s) {
                    continue;
                }
                let open = |s: BlockStateId| !sturdy(s);
                if open(ctx.lv.get(q.below(1))) {
                    continue;
                }
                if Dir::HORIZONTAL.iter().any(|d| open(ctx.lv.get(q.rel(*d)))) {
                    continue;
                }
                ctx.lv.set(q, states().magma);
                placed += 1;
            }
        }
    }
    placed > 0
}

#[allow(clippy::too_many_arguments)]
fn multiface(
    ctx: &mut Ctx,
    block: BlockStateId,
    search: i32,
    floor: bool,
    ceiling: bool,
    wall: bool,
    spread: f32,
    on: &BlockSet,
    pos: Pos,
) -> bool {
    let air_or_water = |s: BlockStateId| blockinfo::is_air(s) || is(s, "minecraft:water");
    if !air_or_water(ctx.lv.get(pos)) {
        return false;
    }
    let mut dirs: Vec<Dir> = Vec::new();
    if ceiling {
        dirs.push(Dir::Up);
    }
    if floor {
        dirs.push(Dir::Down);
    }
    if wall {
        dirs.extend(Dir::HORIZONTAL);
    }
    let list = shuffle(ctx, &dirs);
    if place_growth_if_possible(ctx, block, spread, on, pos, &list) {
        return true;
    }
    for d in list {
        let except: Vec<Dir> = dirs
            .iter()
            .copied()
            .filter(|x| *x != d.opposite())
            .collect();
        let list2 = shuffle(ctx, &except);
        for _ in 0..search {
            // 1.21.11's `MultifaceGrowthFeature` re-sets to `origin + dir` on
            // every step (`MutableBlockPos.setWithOffset(origin, dir)`), so
            // this walk does not accumulate; the loop is the game's own.
            let p = pos.rel(d);
            let s = ctx.lv.get(p);
            if !air_or_water(s) && blockinfo::block_of(s) != blockinfo::block_of(block) {
                break;
            }
            if place_growth_if_possible(ctx, block, spread, on, p, &list2) {
                return true;
            }
        }
    }
    false
}

/// `MultifaceGrowthFeature.placeGrowthIfPossible`.
fn place_growth_if_possible(
    ctx: &mut Ctx,
    block: BlockStateId,
    spread: f32,
    on: &BlockSet,
    pos: Pos,
    dirs: &[Dir],
) -> bool {
    for d in dirs {
        if on.contains(ctx.lv.get(pos.rel(*d))) {
            let here = ctx.lv.get(pos);
            let Some(s) = multiface_state_for_placement(block, here, ctx.lv.get(pos.rel(*d)), *d)
            else {
                return false;
            };
            ctx.lv.set(pos, s);
            if ctx.r.next_float() < spread {
                spread_from_face_toward_random_direction(ctx, block, s, pos, *d);
            }
            return true;
        }
    }
    false
}

/// `MultifaceBlock.getStateForPlacement(state, getter, pos, direction)`.
fn multiface_state_for_placement(
    block: BlockStateId,
    state: BlockStateId,
    attach: BlockStateId,
    direction: Dir,
) -> Option<BlockStateId> {
    if !multi_valid_for_placement(block, state, attach, direction) {
        return None;
    }
    let base = if blockinfo::block_of(state) == blockinfo::block_of(block) {
        state
    } else if is_water_source(state) {
        blockinfo::with_prop(block, "waterlogged", "true")
    } else {
        block
    };
    Some(blockinfo::with_prop(base, direction.name(), "true"))
}

/// `MultifaceBlock.isValidStateForPlacement`, with `canAttachTo`'s face test
/// reduced to a full-collision test (no per-face shapes here).
fn multi_valid_for_placement(
    block: BlockStateId,
    state: BlockStateId,
    attach: BlockStateId,
    direction: Dir,
) -> bool {
    if blockinfo::block_of(state) == blockinfo::block_of(block) && has_face(state, direction) {
        return false;
    }
    sturdy(attach)
}

/// `MultifaceBlock.hasFace`.
fn has_face(state: BlockStateId, direction: Dir) -> bool {
    prop(state, direction.name()) == Some("true")
}

/// `MultifaceSpreader.spreadFromFaceTowardRandomDirection`.
fn spread_from_face_toward_random_direction(
    ctx: &mut Ctx,
    block: BlockStateId,
    state: BlockStateId,
    pos: Pos,
    direction: Dir,
) {
    for d2 in shuffle(ctx, &Dir::ALL) {
        if spread_from_face_toward_direction(ctx, block, state, pos, direction, d2) {
            return;
        }
    }
}

/// `MultifaceSpreader.spreadFromFaceTowardDirection`.
fn spread_from_face_toward_direction(
    ctx: &mut Ctx,
    block: BlockStateId,
    state: BlockStateId,
    pos: Pos,
    direction: Dir,
    d2: Dir,
) -> bool {
    let Some((sp, face)) = spread_from_face(ctx, block, state, pos, direction, d2) else {
        return false;
    };
    let here = ctx.lv.get(sp);
    let Some(new_state) =
        multiface_state_for_placement(block, here, ctx.lv.get(sp.rel(face)), face)
    else {
        return false;
    };
    ctx.lv.set(sp, new_state)
}

/// `getSpreadFromFaceTowardDirection` + `canSpreadInto`; returns the position
/// and face of the first spread type that can be placed.
fn spread_from_face(
    ctx: &Ctx,
    block: BlockStateId,
    state: BlockStateId,
    pos: Pos,
    direction: Dir,
    d2: Dir,
) -> Option<(Pos, Dir)> {
    if d2.axis() == direction.axis() {
        return None;
    }
    if !(has_face(state, direction) && !has_face(state, d2)) {
        return None;
    }
    // SAME_POSITION, SAME_PLANE, WRAP_AROUND.
    let candidates = [
        (pos, d2),
        (pos.rel(d2), direction),
        (pos.rel(d2).rel(direction), d2.opposite()),
    ];
    for (sp, face) in candidates {
        if can_spread_into(ctx, block, sp, face) {
            return Some((sp, face));
        }
    }
    None
}

/// `DefaultSpreaderConfig.canSpreadInto`: the cell may be replaced and the
/// face attaches.
fn can_spread_into(ctx: &Ctx, block: BlockStateId, sp: Pos, face: Dir) -> bool {
    let s = ctx.lv.get(sp);
    let replaceable = blockinfo::is_air(s)
        || blockinfo::block_of(s) == blockinfo::block_of(block)
        || (is(s, "minecraft:water") && is_water_source(s));
    replaceable && multi_valid_for_placement(block, s, ctx.lv.get(sp.rel(face)), face)
}

/// `Util.shuffledCopy`.
pub(crate) fn shuffle<T: Clone>(ctx: &mut Ctx, v: &[T]) -> Vec<T> {
    let mut out = v.to_vec();
    let mut i = out.len();
    while i > 1 {
        let j = ctx.r.next_int_bounded(i as i32) as usize;
        out.swap(i - 1, j);
        i -= 1;
    }
    out
}

/// `DesertWellFeature`.
fn desert_well(ctx: &mut Ctx, origin: Pos) -> bool {
    let st = states();
    // `blockPos = origin.above()`, then down while the block is empty.
    let mut p = origin.above(1);
    while ctx.lv.is_air(p) && p.y > ctx.lv.min_y() + 2 {
        p = p.below(1);
    }
    if blockinfo::block_of(ctx.lv.get(p)) != blockinfo::block_of(st.sand) {
        return false;
    }
    // The well needs two solid layers under its whole footprint.
    for i in -2..=2 {
        for j in -2..=2 {
            if ctx.lv.is_air(p.offset(i, -1, j)) && ctx.lv.is_air(p.offset(i, -2, j)) {
                return false;
            }
        }
    }
    for i in -2..=0 {
        for j in -2..=2 {
            for k in -2..=2 {
                ctx.lv.set(p.offset(j, i, k), st.sandstone);
            }
        }
    }
    ctx.lv.set(p, st.water);
    for d in Dir::HORIZONTAL {
        ctx.lv.set(p.rel(d), st.water);
    }
    let p2 = p.below(1);
    ctx.lv.set(p2, st.sand);
    for d in Dir::HORIZONTAL {
        ctx.lv.set(p2.rel(d), st.sand);
    }
    for j in -2..=2 {
        for k in -2..=2 {
            if j == -2 || j == 2 || k == -2 || k == 2 {
                ctx.lv.set(p.offset(j, 1, k), st.sandstone);
            }
        }
    }
    ctx.lv.set(p.offset(2, 1, 0), st.sandstone_slab);
    ctx.lv.set(p.offset(-2, 1, 0), st.sandstone_slab);
    ctx.lv.set(p.offset(0, 1, 2), st.sandstone_slab);
    ctx.lv.set(p.offset(0, 1, -2), st.sandstone_slab);
    for j in -1..=1 {
        for k in -1..=1 {
            let s = if j == 0 && k == 0 {
                st.sandstone
            } else {
                st.sandstone_slab
            };
            ctx.lv.set(p.offset(j, 4, k), s);
        }
    }
    for j in 1..=3 {
        ctx.lv.set(p.offset(-1, j, -1), st.sandstone);
        ctx.lv.set(p.offset(-1, j, 1), st.sandstone);
        ctx.lv.set(p.offset(1, j, -1), st.sandstone);
        ctx.lv.set(p.offset(1, j, 1), st.sandstone);
    }
    // Two suspicious-sand blocks, each at a random one of the well's five
    // central columns, one and two layers below it.
    let list = [
        p,
        p.rel(Dir::East),
        p.rel(Dir::South),
        p.rel(Dir::West),
        p.rel(Dir::North),
    ];
    let a = list[ctx.r.next_int_bounded(5) as usize].below(1);
    let b = list[ctx.r.next_int_bounded(5) as usize].below(2);
    ctx.lv.set(a, st.suspicious_sand);
    ctx.lv.set(b, st.suspicious_sand);
    true
}

/// `MonsterRoomFeature` (dungeons). Loot tables and the spawner's entity are
/// not generated — the engine has no block entities yet.
fn monster_room(ctx: &mut Ctx, origin: Pos) -> bool {
    let s = states();
    // `Feature.isReplaceable(FEATURES_CANNOT_REPLACE)`: write only into blocks
    // the pack lets features replace.
    let cannot = ctx.d.tags.features_cannot_replace.clone();
    let safe = |ctx: &mut Ctx, p: Pos, v: BlockStateId| {
        if !cannot.contains(ctx.lv.get(p)) {
            ctx.lv.set(p, v);
        }
    };
    let j = ctx.r.next_int_bounded(2) + 2;
    let (k, l) = (-j - 1, j + 1);
    let o = ctx.r.next_int_bounded(2) + 2;
    let (p_, q) = (-o - 1, o + 1);
    let mut r = 0;
    for sx in k..=l {
        for ty in -1..=4 {
            for sz in p_..=q {
                let at = origin.offset(sx, ty, sz);
                let solid = legacy_solid(ctx.lv.get(at));
                if ty == -1 && !solid {
                    return false;
                }
                if ty == 4 && !solid {
                    return false;
                }
                if (sx == k || sx == l || sz == p_ || sz == q)
                    && ty == 0
                    && ctx.lv.is_air(at)
                    && ctx.lv.is_air(at.above(1))
                {
                    r += 1;
                }
            }
        }
    }
    if !(1..=5).contains(&r) {
        return false;
    }
    for sx in k..=l {
        for ty in (0..=3).rev().chain([-1]) {
            for sz in p_..=q {
                let at = origin.offset(sx, ty, sz);
                let cur = ctx.lv.get(at);
                let is_chest = is(cur, "minecraft:chest");
                if sx == k || ty == -1 || sz == p_ || sx == l || ty == 4 || sz == q {
                    if at.y >= ctx.lv.min_y() && !legacy_solid(ctx.lv.get(at.below(1))) {
                        ctx.lv.set(at, states().cave_air);
                    } else if legacy_solid(cur) && !is_chest {
                        if ty == -1 && ctx.r.next_int_bounded(4) != 0 {
                            safe(ctx, at, s.mossy_cobblestone);
                        } else {
                            safe(ctx, at, s.cobblestone);
                        }
                    }
                } else if !is_chest && !is(cur, "minecraft:spawner") {
                    safe(ctx, at, states().cave_air);
                }
            }
        }
    }
    'outer: for _ in 0..2 {
        for _ in 0..3 {
            let x = origin.x + ctx.r.next_int_bounded(j * 2 + 1) - j;
            let z = origin.z + ctx.r.next_int_bounded(o * 2 + 1) - o;
            let at = Pos::new(x, origin.y, z);
            if ctx.lv.is_air(at) {
                let mut solid_dirs = 0;
                for d in Dir::HORIZONTAL {
                    if legacy_solid(ctx.lv.get(at.rel(d))) {
                        solid_dirs += 1;
                    }
                }
                if solid_dirs == 1 {
                    let chest = reorient_chest(ctx.lv, at, s.chest);
                    safe(ctx, at, chest);
                    break 'outer;
                }
            }
        }
    }
    safe(ctx, origin, s.spawner);
    // `randomEntityId`: the spawner's mob is drawn here even though the engine
    // keeps no block entity for it.
    let _ = ctx.r.next_int_bounded(4);
    true
}

/// `StructurePiece.reorient` for a chest.
fn reorient_chest(lv: &Level, pos: Pos, chest: BlockStateId) -> BlockStateId {
    let mut facing: Option<Dir> = None;
    for d in Dir::HORIZONTAL {
        let other = lv.get(pos.rel(d));
        if is(other, "minecraft:chest") {
            return chest;
        }
        if solid_render(other) {
            if facing.is_some() {
                facing = None;
                break;
            }
            facing = Some(d);
        }
    }
    let f = if let Some(d) = facing {
        d.opposite()
    } else {
        let mut f = Dir::parse(prop(chest, "facing").unwrap_or("north")).unwrap_or(Dir::North);
        if solid_render(lv.get(pos.rel(f))) {
            f = f.opposite();
        }
        if solid_render(lv.get(pos.rel(f))) {
            f = f.clockwise();
        }
        if solid_render(lv.get(pos.rel(f))) {
            f = f.opposite();
        }
        f
    };
    blockinfo::with_prop(chest, "facing", f.name())
}

/// `GeodeFeature`. `cannot_replace` is consulted before every write, so the
/// geode never overwrites bedrock, spawners and the like.
fn geode(ctx: &mut Ctx, cfg: &GeodeConfig, origin: Pos) -> bool {
    let k = cfg.distribution.sample(ctx.r);
    let noise = NormalNoise::create_legacy(ctx.lv.core.seed, -4, &[1.0]);
    let d = k as f64 / cfg.outer_wall.max_value() as f64;
    let e = 1.0 / cfg.filling_layer.sqrt();
    let f = 1.0 / (cfg.inner_layer + d).sqrt();
    let g = 1.0 / (cfg.middle_layer + d).sqrt();
    let h = 1.0 / (cfg.outer_layer + d).sqrt();
    let l = 1.0 / (cfg.crack_base + ctx.r.next_double() / 2.0 + if k > 3 { d } else { 0.0 }).sqrt();
    let crack = ctx.r.next_float() < cfg.crack_chance as f32;

    let mut points: Vec<(Pos, i32)> = Vec::new();
    let mut m = 0;
    for _ in 0..k {
        let o = cfg.outer_wall.sample(ctx.r);
        let p = cfg.outer_wall.sample(ctx.r);
        let q = cfg.outer_wall.sample(ctx.r);
        let at = origin.offset(o, p, q);
        let state = ctx.lv.get(at);
        if blockinfo::is_air(state) || cfg.invalid_blocks.contains(state) {
            m += 1;
            if m > cfg.invalid_threshold {
                return false;
            }
        }
        points.push((at, cfg.point_offset.sample(ctx.r)));
    }

    let mut cracks: Vec<Pos> = Vec::new();
    if crack {
        let n = ctx.r.next_int_bounded(4);
        let o = k * 2 + 1;
        match n {
            0 => {
                cracks.push(origin.offset(o, 7, 0));
                cracks.push(origin.offset(o, 5, 0));
                cracks.push(origin.offset(o, 1, 0));
            }
            1 => {
                cracks.push(origin.offset(0, 7, o));
                cracks.push(origin.offset(0, 5, o));
                cracks.push(origin.offset(0, 1, o));
            }
            2 => {
                cracks.push(origin.offset(o, 7, o));
                cracks.push(origin.offset(o, 5, o));
                cracks.push(origin.offset(o, 1, o));
            }
            _ => {
                cracks.push(origin.offset(0, 7, 0));
                cracks.push(origin.offset(0, 5, 0));
                cracks.push(origin.offset(0, 1, 0));
            }
        }
    }

    let mut placements: Vec<Pos> = Vec::new();
    let (i, j) = (cfg.min_gen_offset, cfg.max_gen_offset);
    // `betweenClosed`: x fastest, then y, then z.
    for z in i..=j {
        for y in i..=j {
            for x in i..=j {
                let at = origin.offset(x, y, z);
                let r =
                    noise.get_value(at.x as f64, at.y as f64, at.z as f64) * cfg.noise_multiplier;
                let mut s = 0.0;
                let mut t = 0.0;
                for (p, off) in &points {
                    s += 1.0
                        / ((at.x - p.x).pow(2) as f64
                            + (at.y - p.y).pow(2) as f64
                            + (at.z - p.z).pow(2) as f64
                            + *off as f64)
                            .sqrt()
                        + r;
                }
                for p in &cracks {
                    t += 1.0
                        / ((at.x - p.x).pow(2) as f64
                            + (at.y - p.y).pow(2) as f64
                            + (at.z - p.z).pow(2) as f64
                            + cfg.crack_offset as f64)
                            .sqrt()
                        + r;
                }
                if s < h {
                    continue;
                }
                let cannot = &cfg.cannot_replace;
                let put = |ctx: &mut Ctx, p: Pos, v: BlockStateId| {
                    if !cannot.contains(ctx.lv.get(p)) {
                        ctx.lv.set(p, v);
                    }
                };
                if crack && t >= l && s < e {
                    put(ctx, at, states().cave_air);
                } else if s >= e {
                    let v = cfg.filling.get(ctx.r, at);
                    put(ctx, at, v);
                } else if s >= f {
                    let bl2 = ctx.r.next_float() < cfg.use_alternate as f32;
                    let v = if bl2 {
                        cfg.alternate_inner.get(ctx.r, at)
                    } else {
                        cfg.inner.get(ctx.r, at)
                    };
                    put(ctx, at, v);
                    if (!cfg.require_alternate || bl2)
                        && ctx.r.next_float() < cfg.use_potential as f32
                    {
                        placements.push(at);
                    }
                } else if s >= g {
                    let v = cfg.middle.get(ctx.r, at);
                    put(ctx, at, v);
                } else {
                    let v = cfg.outer.get(ctx.r, at);
                    put(ctx, at, v);
                }
            }
        }
    }

    for at in placements {
        let mut s = cfg.inner_placements
            [ctx.r.next_int_bounded(cfg.inner_placements.len() as i32) as usize];
        for d in Dir::ALL {
            if prop(s, "facing").is_some() {
                s = blockinfo::with_prop(s, "facing", d.name());
            }
            let p = at.rel(d);
            let other = ctx.lv.get(p);
            if prop(s, "waterlogged").is_some() {
                s = blockinfo::with_prop(
                    s,
                    "waterlogged",
                    if is_water_source(other) {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            if blockinfo::is_air(other) || (is(other, "minecraft:water") && is_water_source(other))
            {
                if !cfg.cannot_replace.contains(ctx.lv.get(p)) {
                    ctx.lv.set(p, s);
                }
                break;
            }
        }
    }
    true
}

/// `FossilFeature`. Places one of the pack's fossil templates (spines and
/// skulls) rotated at a random underground height, then its overlay.
fn fossil(ctx: &mut Ctx, cfg: &FossilConfig, origin: Pos) -> bool {
    if cfg.fossil_structures.is_empty() || cfg.overlay_structures.is_empty() {
        return false;
    }
    let rotation = Rotation::random(ctx.r);
    let i = ctx.r.next_int_bounded(cfg.fossil_structures.len() as i32) as usize;
    let template = &cfg.fossil_structures[i];
    let overlay = &cfg.overlay_structures[i];
    let size = rotation_size(rotation, template.size);
    let p2 = origin.offset(-size[0] / 2, 0, -size[2] / 2);
    let mut j = origin.y;
    for k in 0..size[0] {
        for l in 0..size[2] {
            j = j.min(ctx.lv.height(Heightmap::OceanFloorWg, p2.x + k, p2.z + l));
        }
    }
    let k = (j - 15 - ctx.r.next_int_bounded(10)).max(ctx.lv.min_y() + 10);
    let base = zero_position(Pos::new(p2.x, k, p2.z), rotation, template.size);
    let (min, max) = bounding_box(base, rotation, template.size);
    let mut empty = 0;
    for x in [min.x, max.x] {
        for y in [min.y, max.y] {
            for z in [min.z, max.z] {
                let s = ctx.lv.get(Pos::new(x, y, z));
                if blockinfo::is_air(s) || is(s, "minecraft:lava") || is(s, "minecraft:water") {
                    empty += 1;
                }
            }
        }
    }
    if empty > cfg.max_empty_corners {
        return false;
    }
    let bbox = Some((min, max));
    structure::place_in_world(
        ctx.lv,
        base,
        template,
        rotation,
        &cfg.fossil_processors,
        bbox,
        ctx.r,
    );
    structure::place_in_world(
        ctx.lv,
        base,
        overlay,
        rotation,
        &cfg.overlay_processors,
        bbox,
        ctx.r,
    );
    true
}

/// `StructureTemplate.getSize(rotation)`.
fn rotation_size(rotation: Rotation, size: [i32; 3]) -> [i32; 3] {
    match rotation {
        Rotation::Cw90 | Rotation::Ccw90 => [size[2], size[1], size[0]],
        _ => size,
    }
}

fn huge_mushroom(
    ctx: &mut Ctx,
    red: bool,
    cap: &StateProvider,
    stem: &StateProvider,
    radius: i32,
    pos: Pos,
) -> bool {
    let mut height = ctx.r.next_int_bounded(3) + 4;
    if ctx.r.next_int_bounded(12) == 0 {
        height *= 2;
    }
    // isValidPosition
    if pos.y < ctx.lv.min_y() + 1 || pos.y + height + 1 > ctx.lv.max_y() - 1 {
        return false;
    }
    let below = ctx.lv.get(pos.below(1));
    if !ctx.d.tags.dirt.contains(below) && !ctx.d.tags.mushroom_grow_block.contains(below) {
        return false;
    }
    for y in 0..=height {
        let r = if red {
            // getTreeRadiusForHeight(-1, -1, radius, y): always 0.
            0
        } else if y <= 3 {
            0
        } else {
            radius
        };
        for dx in -r..=r {
            for dz in -r..=r {
                let s = ctx.lv.get(pos.offset(dx, y, dz));
                if !blockinfo::is_air(s) && !ctx.d.tags.leaves.contains(s) {
                    return false;
                }
            }
        }
    }
    let by_mush = ctx
        .d
        .block_tags
        .block_tag("minecraft:replaceable_by_mushrooms");
    let put = |ctx: &mut Ctx, p: Pos, s: BlockStateId| {
        let cur = ctx.lv.get(p);
        if blockinfo::is_air(cur) || by_mush.contains(cur) {
            ctx.lv.set(p, s);
        }
    };
    let b = |v: bool| if v { "true" } else { "false" };
    if red {
        for y in height - 3..=height {
            let r = if y < height { radius } else { radius - 1 };
            let inner = radius - 2;
            for dx in -r..=r {
                for dz in -r..=r {
                    let ex = dx == -r || dx == r;
                    let ez = dz == -r || dz == r;
                    if y >= height || ex != ez {
                        let mut s = cap.get(ctx.r, pos);
                        if prop(s, "west").is_some() && prop(s, "up").is_some() {
                            s = blockinfo::with_prop(s, "up", b(y >= height - 1));
                            s = blockinfo::with_prop(s, "west", b(dx < -inner));
                            s = blockinfo::with_prop(s, "east", b(dx > inner));
                            s = blockinfo::with_prop(s, "north", b(dz < -inner));
                            s = blockinfo::with_prop(s, "south", b(dz > inner));
                        }
                        put(ctx, pos.offset(dx, y, dz), s);
                    }
                }
            }
        }
    } else {
        let r = radius;
        for dx in -r..=r {
            for dz in -r..=r {
                let (w, e, n, s_) = (dx == -r, dx == r, dz == -r, dz == r);
                let ex = w || e;
                let ez = n || s_;
                if !ex || !ez {
                    let west = w || ez && dx == 1 - r;
                    let east = e || ez && dx == r - 1;
                    let north = n || ex && dz == 1 - r;
                    let south = s_ || ex && dz == r - 1;
                    let mut s = cap.get(ctx.r, pos);
                    if prop(s, "west").is_some() {
                        s = blockinfo::with_prop(s, "west", b(west));
                        s = blockinfo::with_prop(s, "east", b(east));
                        s = blockinfo::with_prop(s, "north", b(north));
                        s = blockinfo::with_prop(s, "south", b(south));
                    }
                    put(ctx, pos.offset(dx, height, dz), s);
                }
            }
        }
    }
    for y in 0..height {
        let s = stem.get(ctx.r, pos);
        put(ctx, pos.above(y), s);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{count_name, pack, parse_configured, place, stone, World};
    use super::*;
    use crate::vanilla::rng::WorldgenRandom;

    fn setup() -> Option<(String, crate::vanilla::generator::VanillaGenerator)> {
        let pack = pack()?;
        let gen = crate::vanilla::generator::VanillaGenerator::new(&pack, 42).expect("generator");
        Some((pack, gen))
    }

    #[test]
    fn desert_well_places() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        // A sand plateau at y = 64 with stone beneath; the well sinks into it.
        let mut w = World::new(&gen, 0, 0);
        w.fill(-16, 0, -16, 31, 63, 31, stone());
        w.fill(-16, 64, -16, 31, 70, 31, states().sand);
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(r#"{"type":"minecraft:desert_well","config":{}}"#).unwrap(),
        );
        let writes = place(&gen, &w, &cfg, Pos::new(8, 64, 8), 1);
        assert!(
            count_name(&writes, "minecraft:sandstone") > 0,
            "no sandstone"
        );
        assert!(count_name(&writes, "minecraft:water") > 0, "no water");
        assert_eq!(
            count_name(&writes, "minecraft:suspicious_sand"),
            2,
            "expected exactly two suspicious sand blocks"
        );
    }

    #[test]
    fn monster_room_places() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let seed = 1i64;
        let origin = Pos::new(8, 40, 8);
        // Replay the two size draws so the solid shell matches the room.
        let mut rr = WorldgenRandom::xoroshiro(seed);
        let j = rr.next_int_bounded(2) + 2;
        let o = rr.next_int_bounded(2) + 2;
        let (k, l) = (-j - 1, j + 1);
        let (p, q) = (-o - 1, o + 1);
        let mut w = World::new(&gen, 0, 0);
        w.fill(
            origin.x - 6,
            origin.y - 1,
            origin.z - 6,
            origin.x + 6,
            origin.y - 1,
            origin.z + 6,
            stone(),
        );
        w.fill(
            origin.x - 6,
            origin.y + 4,
            origin.z - 6,
            origin.x + 6,
            origin.y + 4,
            origin.z + 6,
            stone(),
        );
        w.fill(
            origin.x - 6,
            origin.y,
            origin.z - 6,
            origin.x + 6,
            origin.y,
            origin.z + 6,
            stone(),
        );
        for sx in k + 1..=l - 1 {
            for sz in p + 1..=q - 1 {
                for y in origin.y..=origin.y + 3 {
                    w.set(origin.x + sx, y, origin.z + sz, BlockStateId::AIR);
                }
            }
        }
        // One opening on the wall, so the room's entrance count is in range.
        w.set(origin.x + k, origin.y, origin.z, BlockStateId::AIR);
        w.set(origin.x + k, origin.y + 1, origin.z, BlockStateId::AIR);
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(r#"{"type":"minecraft:monster_room","config":{}}"#).unwrap(),
        );
        let writes = place(&gen, &w, &cfg, origin, seed);
        assert!(count_name(&writes, "minecraft:spawner") >= 1, "no spawner");
        assert!(
            count_name(&writes, "minecraft:cobblestone") > 0,
            "no cobblestone"
        );
    }

    #[test]
    fn geode_places() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let mut w = World::new(&gen, 0, 0);
        w.fill(-20, 0, -20, 40, 120, 40, stone());
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(
                r##"{"type":"minecraft:geode","config":{
                    "blocks":{
                        "filling_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:air"}},
                        "inner_layer_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:amethyst_block"}},
                        "alternate_inner_layer_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:budding_amethyst"}},
                        "middle_layer_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:calcite"}},
                        "outer_layer_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:smooth_basalt"}},
                        "inner_placements":[{"Name":"minecraft:small_amethyst_bud","Properties":{"facing":"up","waterlogged":"false"}},{"Name":"minecraft:amethyst_cluster","Properties":{"facing":"up","waterlogged":"false"}}],
                        "cannot_replace":"#minecraft:features_cannot_replace",
                        "invalid_blocks":"#minecraft:geode_invalid_blocks"
                    },
                    "layers":{"filling":1.7,"inner_layer":2.2,"middle_layer":3.2,"outer_layer":4.2},
                    "crack":{"base_crack_size":2.0,"crack_point_offset":2,"generate_crack_chance":0.95},
                    "distribution_points":{"type":"minecraft:uniform","min_inclusive":3,"max_inclusive":4},
                    "point_offset":{"type":"minecraft:uniform","min_inclusive":1,"max_inclusive":2},
                    "outer_wall_distance":{"type":"minecraft:uniform","min_inclusive":4,"max_inclusive":6},
                    "invalid_blocks_threshold":1,"max_gen_offset":16,"min_gen_offset":-16,
                    "noise_multiplier":0.05,"placements_require_layer0_alternate":true,
                    "use_alternate_layer0_chance":0.083,"use_potential_placements_chance":0.35
                }}"##,
            )
            .unwrap(),
        );
        let writes = place(&gen, &w, &cfg, Pos::new(8, 50, 8), 3);
        assert!(
            count_name(&writes, "minecraft:amethyst_block") > 0,
            "no amethyst"
        );
        assert!(
            count_name(&writes, "minecraft:smooth_basalt") > 0,
            "no smooth basalt"
        );
        assert!(count_name(&writes, "minecraft:calcite") > 0, "no calcite");
    }

    #[test]
    fn geode_deterministic() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let mut w = World::new(&gen, 0, 0);
        w.fill(-20, 0, -20, 40, 120, 40, stone());
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(
                r##"{"type":"minecraft:geode","config":{
                    "blocks":{
                        "filling_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:air"}},
                        "inner_layer_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:amethyst_block"}},
                        "alternate_inner_layer_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:budding_amethyst"}},
                        "middle_layer_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:calcite"}},
                        "outer_layer_provider":{"type":"minecraft:simple_state_provider","state":{"Name":"minecraft:smooth_basalt"}},
                        "inner_placements":[{"Name":"minecraft:amethyst_cluster","Properties":{"facing":"up","waterlogged":"false"}}],
                        "cannot_replace":"#minecraft:features_cannot_replace",
                        "invalid_blocks":"#minecraft:geode_invalid_blocks"
                    },
                    "layers":{"filling":1.7,"inner_layer":2.2,"middle_layer":3.2,"outer_layer":4.2},
                    "crack":{"base_crack_size":2.0,"crack_point_offset":2,"generate_crack_chance":0.95},
                    "distribution_points":{"type":"minecraft:uniform","min_inclusive":3,"max_inclusive":4},
                    "point_offset":{"type":"minecraft:uniform","min_inclusive":1,"max_inclusive":2},
                    "outer_wall_distance":{"type":"minecraft:uniform","min_inclusive":4,"max_inclusive":6},
                    "invalid_blocks_threshold":1,"max_gen_offset":16,"min_gen_offset":-16,
                    "noise_multiplier":0.05,"placements_require_layer0_alternate":true,
                    "use_alternate_layer0_chance":0.083,"use_potential_placements_chance":0.35
                }}"##,
            )
            .unwrap(),
        );
        let a = place(&gen, &w, &cfg, Pos::new(8, 50, 8), 3);
        let b = place(&gen, &w, &cfg, Pos::new(8, 50, 8), 3);
        assert_eq!(a, b);
    }

    #[test]
    fn monster_room_deterministic() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let seed = 1i64;
        let origin = Pos::new(8, 40, 8);
        let mut w = World::new(&gen, 0, 0);
        w.fill(
            origin.x - 6,
            origin.y - 1,
            origin.z - 6,
            origin.x + 6,
            origin.y - 1,
            origin.z + 6,
            stone(),
        );
        w.fill(
            origin.x - 6,
            origin.y + 4,
            origin.z - 6,
            origin.x + 6,
            origin.y + 4,
            origin.z + 6,
            stone(),
        );
        w.fill(
            origin.x - 6,
            origin.y,
            origin.z - 6,
            origin.x + 6,
            origin.y,
            origin.z + 6,
            stone(),
        );
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(r#"{"type":"minecraft:monster_room","config":{}}"#).unwrap(),
        );
        let a = place(&gen, &w, &cfg, origin, seed);
        let b = place(&gen, &w, &cfg, origin, seed);
        assert_eq!(a, b);
    }

    #[test]
    fn desert_well_deterministic() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let mut w = World::new(&gen, 0, 0);
        w.fill(-16, 0, -16, 31, 63, 31, stone());
        w.fill(-16, 64, -16, 31, 70, 31, states().sand);
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(r#"{"type":"minecraft:desert_well","config":{}}"#).unwrap(),
        );
        let a = place(&gen, &w, &cfg, Pos::new(8, 64, 8), 7);
        let b = place(&gen, &w, &cfg, Pos::new(8, 64, 8), 7);
        assert_eq!(a, b);
    }

    /// A solid stone 3×3 region the fossil templates can sink into.
    fn fossil_world(gen: &crate::vanilla::generator::VanillaGenerator) -> World {
        let mut w = World::new(gen, 0, 0);
        w.fill(-16, 0, -16, 31, 48, 31, stone());
        w
    }

    #[test]
    fn fossil_places() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let w = fossil_world(&gen);
        // The pack's own configured feature, with all eight templates and
        // the real processor lists.
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse("\"minecraft:fossil_coal\"").unwrap(),
        );
        let origin = Pos::new(8, 49, 8);
        let seed = (0..256)
            .find(|s| count_name(&place(&gen, &w, &cfg, origin, *s), "minecraft:bone_block") > 0)
            .expect("no seed in 0..256 placed a fossil");
        let writes = place(&gen, &w, &cfg, origin, seed);
        assert!(
            count_name(&writes, "minecraft:bone_block") > 0,
            "no bone block from the base template"
        );
        assert!(
            count_name(&writes, "minecraft:coal_ore") > 0,
            "no coal ore from the coal overlay"
        );
    }

    #[test]
    fn fossil_diamonds_overlay_replaces_coal() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let w = fossil_world(&gen);
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse("\"minecraft:fossil_diamonds\"").unwrap(),
        );
        let origin = Pos::new(8, 49, 8);
        let seed = (0..256)
            .find(|s| {
                count_name(
                    &place(&gen, &w, &cfg, origin, *s),
                    "minecraft:deepslate_diamond_ore",
                ) > 0
            })
            .expect("no seed in 0..256 produced a diamond ore");
        let writes = place(&gen, &w, &cfg, origin, seed);
        assert!(
            count_name(&writes, "minecraft:deepslate_diamond_ore") > 0,
            "the rule processor did not turn surviving coal into diamond ore"
        );
        assert_eq!(
            count_name(&writes, "minecraft:coal_ore"),
            0,
            "a surviving coal block was not converted"
        );
    }

    #[test]
    fn fossil_deterministic() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let w = fossil_world(&gen);
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse("\"minecraft:fossil_coal\"").unwrap(),
        );
        let origin = Pos::new(8, 49, 8);
        let a = place(&gen, &w, &cfg, origin, 12345);
        let b = place(&gen, &w, &cfg, origin, 12345);
        assert_eq!(a, b);
    }

    fn dripstone() -> BlockStateId {
        blockinfo::parse_state("minecraft:dripstone_block").unwrap()
    }

    #[test]
    fn pointed_dripstone_places() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let mut w = World::new(&gen, 0, 0);
        // A base below the origin: the tip grows up from it.
        w.set(8, 39, 8, dripstone());
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(
                r#"{"type":"minecraft:pointed_dripstone","config":{
                    "chance_of_directional_spread":0.7,
                    "chance_of_spread_radius2":0.5,
                    "chance_of_spread_radius3":0.5,
                    "chance_of_taller_dripstone":0.2
                }}"#,
            )
            .unwrap(),
        );
        let writes = place(&gen, &w, &cfg, Pos::new(8, 40, 8), 7);
        assert!(
            count_name(&writes, "minecraft:pointed_dripstone") > 0,
            "no pointed dripstone"
        );
    }

    #[test]
    fn pointed_dripstone_deterministic() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let mut w = World::new(&gen, 0, 0);
        w.set(8, 39, 8, dripstone());
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(
                r#"{"type":"minecraft:pointed_dripstone","config":{
                    "chance_of_directional_spread":0.7,
                    "chance_of_spread_radius2":0.5,
                    "chance_of_spread_radius3":0.5,
                    "chance_of_taller_dripstone":0.2
                }}"#,
            )
            .unwrap(),
        );
        let a = place(&gen, &w, &cfg, Pos::new(8, 40, 8), 11);
        let b = place(&gen, &w, &cfg, Pos::new(8, 40, 8), 11);
        assert_eq!(a, b);
    }

    /// A cave: stone floor below, stone ceiling above.
    fn cave_world(
        gen: &crate::vanilla::generator::VanillaGenerator,
        floor: i32,
        ceiling: i32,
    ) -> World {
        let mut w = World::new(gen, 0, 0);
        w.fill(-16, 0, -16, 31, floor, 31, stone());
        w.fill(-16, ceiling, -16, 31, 90, 31, stone());
        w
    }

    #[test]
    fn dripstone_cluster_places() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let w = cave_world(&gen, 30, 51);
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse("\"minecraft:dripstone_cluster\"").unwrap(),
        );
        let origin = Pos::new(8, 40, 8);
        let seed = (0..256)
            .find(|s| {
                let wr = place(&gen, &w, &cfg, origin, *s);
                count_name(&wr, "minecraft:pointed_dripstone") > 0
                    || count_name(&wr, "minecraft:dripstone_block") > 0
            })
            .expect("no seed in 0..256 placed a dripstone cluster");
        let writes = place(&gen, &w, &cfg, origin, seed);
        assert!(
            count_name(&writes, "minecraft:pointed_dripstone") > 0
                || count_name(&writes, "minecraft:dripstone_block") > 0,
            "no dripstone cluster blocks"
        );
    }

    #[test]
    fn large_dripstone_places() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let w = cave_world(&gen, 30, 61);
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse("\"minecraft:large_dripstone\"").unwrap(),
        );
        let origin = Pos::new(8, 45, 8);
        let seed = (0..256)
            .find(|s| {
                count_name(
                    &place(&gen, &w, &cfg, origin, *s),
                    "minecraft:dripstone_block",
                ) > 0
            })
            .expect("no seed in 0..256 placed a large dripstone");
        let writes = place(&gen, &w, &cfg, origin, seed);
        assert!(
            count_name(&writes, "minecraft:dripstone_block") > 0,
            "no large dripstone blocks"
        );
    }

    #[test]
    fn dripstone_deterministic() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let w = cave_world(&gen, 30, 61);
        let cluster = parse_configured(
            &gen,
            &pack,
            &Json::parse("\"minecraft:dripstone_cluster\"").unwrap(),
        );
        let large = parse_configured(
            &gen,
            &pack,
            &Json::parse("\"minecraft:large_dripstone\"").unwrap(),
        );
        let origin = Pos::new(8, 45, 8);
        assert_eq!(
            place(&gen, &w, &cluster, origin, 99),
            place(&gen, &w, &cluster, origin, 99)
        );
        assert_eq!(
            place(&gen, &w, &large, origin, 99),
            place(&gen, &w, &large, origin, 99)
        );
    }

    /// Stone everywhere but one air cell, so the growth attaches to a stone
    /// face and the spreader can add a second face to the same cell.
    fn glow_lichen_world(gen: &crate::vanilla::generator::VanillaGenerator) -> World {
        let mut w = World::new(gen, 0, 0);
        w.fill(-16, 0, -16, 31, 70, 31, stone());
        w.set(8, 40, 8, BlockStateId::AIR);
        w
    }

    fn max_faces(writes: &[(i32, i32, i32, BlockStateId)]) -> usize {
        const FACES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
        writes
            .iter()
            .filter(|(_, _, _, s)| blockinfo::name(*s) == "minecraft:glow_lichen")
            .map(|(_, _, _, s)| {
                FACES
                    .iter()
                    .filter(|f| blockinfo::prop(*s, f) == Some("true"))
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn glow_lichen_spreads() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let w = glow_lichen_world(&gen);
        // Force the spread draw so the multiface spreader must run.
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse(
                r#"{"type":"minecraft:multiface_growth","config":{
                    "block":"minecraft:glow_lichen",
                    "can_be_placed_on":["minecraft:stone"],
                    "can_place_on_ceiling":true,
                    "can_place_on_floor":false,
                    "can_place_on_wall":true,
                    "chance_of_spreading":1.0,
                    "search_range":20
                }}"#,
            )
            .unwrap(),
        );
        let writes = place(&gen, &w, &cfg, Pos::new(8, 40, 8), 5);
        assert!(
            count_name(&writes, "minecraft:glow_lichen") > 0,
            "no glow lichen placed"
        );
        assert!(
            max_faces(&writes) >= 2,
            "the spreader did not add a second face"
        );
    }

    #[test]
    fn glow_lichen_deterministic() {
        let Some((pack, gen)) = setup() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let w = glow_lichen_world(&gen);
        let cfg = parse_configured(
            &gen,
            &pack,
            &Json::parse("\"minecraft:glow_lichen\"").unwrap(),
        );
        let a = place(&gen, &w, &cfg, Pos::new(8, 40, 8), 3);
        let b = place(&gen, &w, &cfg, Pos::new(8, 40, 8), 3);
        assert_eq!(a, b);
    }
}
