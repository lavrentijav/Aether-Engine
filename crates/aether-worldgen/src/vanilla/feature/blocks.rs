//! What features ask of blocks: can this plant survive here (`canSurvive`),
//! does this predicate hold (`BlockPredicate`, `RuleTest`), which state does
//! this provider give (`BlockStateProvider`).
//!
//! `canSurvive` is the one place this module cannot read from the pack: it is
//! code on each block class. The rules below are ported from those classes
//! for every block the overworld's features place, keyed by block name; any
//! other block survives anywhere, as a plain `Block` does.

use std::sync::Arc;

use aether_world::registry::blocks;
use aether_world::BlockStateId;

use super::super::blockinfo::{self, prop};
use super::super::density::{BuildError, NoiseRegistry};
use super::super::json::Json;
use super::super::noise::NormalNoise;
use super::super::providers::IntProvider;
use super::super::rng::{Rng, WorldgenRandom};
use super::super::surface::parse_block_state;
use super::super::tags::{BlockSet, Tags};
use super::level::{Dir, Level, Pos};

/// The block tags feature code consults by name.
pub struct CommonTags {
    pub dirt: BlockSet,
    pub sand: BlockSet,
    pub logs: BlockSet,
    pub leaves: BlockSet,
    pub replaceable: BlockSet,
    pub replaceable_by_trees: BlockSet,
    pub base_stone_overworld: BlockSet,
    pub mushroom_grow_block: BlockSet,
    pub dry_vegetation_may_place_on: BlockSet,
    pub bamboo_plantable_on: BlockSet,
    pub snow_can_survive_on: BlockSet,
    pub snow_cannot_survive_on: BlockSet,
    pub jungle_logs: BlockSet,
    pub small_dripleaf_placeable: BlockSet,
    pub big_dripleaf_placeable: BlockSet,
    pub features_cannot_replace: BlockSet,
    pub ice: BlockSet,
}

impl CommonTags {
    /// Resolve every tag above.
    pub fn new(t: &Tags) -> Self {
        Self {
            dirt: t.block_tag("minecraft:dirt"),
            sand: t.block_tag("minecraft:sand"),
            logs: t.block_tag("minecraft:logs"),
            leaves: t.block_tag("minecraft:leaves"),
            replaceable: t.block_tag("minecraft:replaceable"),
            replaceable_by_trees: t.block_tag("minecraft:replaceable_by_trees"),
            base_stone_overworld: t.block_tag("minecraft:base_stone_overworld"),
            mushroom_grow_block: t.block_tag("minecraft:mushroom_grow_block"),
            dry_vegetation_may_place_on: t.block_tag("minecraft:dry_vegetation_may_place_on"),
            bamboo_plantable_on: t.block_tag("minecraft:bamboo_plantable_on"),
            snow_can_survive_on: t.block_tag("minecraft:snow_layer_can_survive_on"),
            snow_cannot_survive_on: t.block_tag("minecraft:snow_layer_cannot_survive_on"),
            jungle_logs: t.block_tag("minecraft:jungle_logs"),
            small_dripleaf_placeable: t.block_tag("minecraft:small_dripleaf_placeable"),
            big_dripleaf_placeable: t.block_tag("minecraft:big_dripleaf_placeable"),
            features_cannot_replace: t.block_tag("minecraft:features_cannot_replace"),
            ice: t.block_tag("minecraft:ice"),
        }
    }
}

/// `isFaceSturdy(UP)` and friends, for the full-cube blocks worldgen walks
/// on. Partial shapes are told apart by name.
pub fn sturdy(s: BlockStateId) -> bool {
    if !blockinfo::blocks_motion(s) {
        return false;
    }
    let n = blockinfo::name(s);
    if n.ends_with("_slab") {
        return prop(s, "type") == Some("double");
    }
    !(n.ends_with("_stairs")
        || n.ends_with("_fence")
        || n.ends_with("_wall")
        || n.ends_with("_pane")
        || n.ends_with("_carpet")
        || n.ends_with("_fence_gate")
        || n.ends_with("_trapdoor")
        || n.ends_with("_door")
        || n == "minecraft:snow"
        || n == "minecraft:iron_bars"
        || n == "minecraft:chain"
        || n == "minecraft:cactus"
        || n == "minecraft:bamboo"
        || n == "minecraft:dirt_path"
        || n == "minecraft:farmland"
        || n.contains("dripleaf")
        || n == "minecraft:pointed_dripstone")
}

/// `isSolidRender`: an opaque full cube.
pub fn solid_render(s: BlockStateId) -> bool {
    blocks::props_of_state(s).map(|p| p.solid).unwrap_or(false)
}

/// `BlockState.isSolid()` — the legacy "solid" flag.
pub fn legacy_solid(s: BlockStateId) -> bool {
    blockinfo::blocks_motion(s)
}

/// The amount of water (8 = a source or a waterlogged block).
pub fn water_amount(s: BlockStateId) -> i32 {
    if !blockinfo::has_water(s) {
        return 0;
    }
    if blockinfo::name(s) == "minecraft:water" {
        match prop(s, "level").and_then(|v| v.parse::<i32>().ok()) {
            Some(0) | None => 8,
            Some(l) if l >= 8 => 8,
            Some(l) => 8 - l,
        }
    } else {
        8
    }
}

/// `FluidState.isSourceOfType(WATER)`.
pub fn is_water_source(s: BlockStateId) -> bool {
    water_amount(s) == 8
}

/// `canSurvive` for the blocks features place.
pub fn can_survive(lv: &Level, t: &CommonTags, s: BlockStateId, p: Pos) -> bool {
    let name = blockinfo::name(s);
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    let below = lv.get(p.below(1));
    let upper = prop(s, "half") == Some("upper");
    let dirt_or_farmland =
        |b: BlockStateId| t.dirt.contains(b) || blockinfo::name(b) == "minecraft:farmland";
    match short {
        // VegetationBlock and its plain subclasses.
        "short_grass" | "fern" | "bush" | "firefly_bush" | "sweet_berry_bush" | "pink_petals"
        | "wildflowers" | "dandelion" | "poppy" | "blue_orchid" | "allium" | "azure_bluet"
        | "red_tulip" | "orange_tulip" | "white_tulip" | "pink_tulip" | "oxeye_daisy"
        | "cornflower" | "lily_of_the_valley" | "torchflower" | "open_eyeblossom"
        | "closed_eyeblossom" | "oak_sapling" | "spruce_sapling" | "birch_sapling"
        | "jungle_sapling" | "acacia_sapling" | "cherry_sapling" | "dark_oak_sapling"
        | "pale_oak_sapling" => dirt_or_farmland(below),
        "wither_rose" => {
            dirt_or_farmland(below)
                || matches!(
                    blockinfo::name(below),
                    "minecraft:netherrack" | "minecraft:soul_sand" | "minecraft:soul_soil"
                )
        }
        "azalea" | "flowering_azalea" => {
            dirt_or_farmland(below) || blockinfo::name(below) == "minecraft:clay"
        }
        // DoublePlantBlock: the lower half is a VegetationBlock, the upper
        // half needs its lower half.
        "tall_grass" | "large_fern" | "sunflower" | "lilac" | "rose_bush" | "peony"
        | "pitcher_plant" => {
            if upper {
                blockinfo::block_of(below) == blockinfo::block_of(s)
                    && prop(below, "half") == Some("lower")
            } else {
                dirt_or_farmland(below)
            }
        }
        "dead_bush" | "short_dry_grass" => t.dry_vegetation_may_place_on.contains(below),
        "tall_dry_grass" => t.dry_vegetation_may_place_on.contains(below),
        "brown_mushroom" | "red_mushroom" => {
            // Decoration runs before lighting, so the brightness check
            // always passes.
            t.mushroom_grow_block.contains(below) || solid_render(below)
        }
        "cactus" => {
            for d in Dir::HORIZONTAL {
                let n = lv.get(p.rel(d));
                if legacy_solid(n) || blockinfo::is_lava(n) {
                    return false;
                }
            }
            (blockinfo::name(below) == "minecraft:cactus" || t.sand.contains(below))
                && !blockinfo::has_fluid(lv.get(p.above(1)))
        }
        "cactus_flower" => {
            blockinfo::name(below) == "minecraft:cactus"
                || blockinfo::name(below) == "minecraft:farmland"
                || sturdy(below)
        }
        "sugar_cane" => {
            if blockinfo::name(below) == "minecraft:sugar_cane" {
                return true;
            }
            if t.dirt.contains(below) || t.sand.contains(below) {
                let b = p.below(1);
                for d in Dir::HORIZONTAL {
                    let n = lv.get(b.rel(d));
                    if blockinfo::has_water(n) || blockinfo::name(n) == "minecraft:frosted_ice" {
                        return true;
                    }
                }
            }
            false
        }
        "lily_pad" => {
            (blockinfo::has_water(below) || t.ice.contains(below))
                && !blockinfo::has_fluid(lv.get(p))
        }
        "seagrass" => sturdy(below) && blockinfo::name(below) != "minecraft:magma_block",
        "tall_seagrass" => {
            if upper {
                blockinfo::name(below) == "minecraft:tall_seagrass"
                    && prop(below, "half") == Some("lower")
            } else {
                sturdy(below)
                    && blockinfo::name(below) != "minecraft:magma_block"
                    && is_water_source(lv.get(p))
            }
        }
        "kelp" | "kelp_plant" => {
            let b = blockinfo::name(below);
            b != "minecraft:magma_block"
                && (b == "minecraft:kelp" || b == "minecraft:kelp_plant" || sturdy(below))
        }
        "sea_pickle" => sturdy(below) || blockinfo::blocks_motion(below),
        "tube_coral" | "brain_coral" | "bubble_coral" | "fire_coral" | "horn_coral"
        | "tube_coral_fan" | "brain_coral_fan" | "bubble_coral_fan" | "fire_coral_fan"
        | "horn_coral_fan" | "dead_tube_coral" | "dead_brain_coral" | "dead_bubble_coral"
        | "dead_fire_coral" | "dead_horn_coral" => sturdy(below),
        "snow" => {
            if t.snow_cannot_survive_on.contains(below) {
                false
            } else if t.snow_can_survive_on.contains(below) {
                true
            } else {
                sturdy(below)
                    || (blockinfo::name(below) == "minecraft:snow"
                        && prop(below, "layers") == Some("8"))
            }
        }
        "leaf_litter" => sturdy(below),
        "bamboo" | "bamboo_sapling" => t.bamboo_plantable_on.contains(below),
        "small_dripleaf" => {
            if upper {
                blockinfo::name(below) == "minecraft:small_dripleaf"
            } else {
                t.small_dripleaf_placeable.contains(below)
                    || (is_water_source(lv.get(p)) && sturdy(below))
            }
        }
        "big_dripleaf" | "big_dripleaf_stem" => {
            let b = blockinfo::name(below);
            b == "minecraft:big_dripleaf_stem"
                || b == "minecraft:big_dripleaf"
                || t.big_dripleaf_placeable.contains(below)
        }
        "spore_blossom" | "hanging_roots" => sturdy(lv.get(p.above(1))),
        "cave_vines" | "cave_vines_plant" => {
            let a = lv.get(p.above(1));
            let n = blockinfo::name(a);
            n == "minecraft:cave_vines" || n == "minecraft:cave_vines_plant" || sturdy(a)
        }
        "pale_hanging_moss" => {
            let a = lv.get(p.above(1));
            blockinfo::name(a) == "minecraft:pale_hanging_moss" || sturdy(a) || t.leaves.contains(a)
        }
        "pale_moss_carpet" | "moss_carpet" => !blockinfo::is_air(below),
        "cocoa" => {
            let facing = prop(s, "facing").and_then(Dir::parse).unwrap_or(Dir::North);
            t.jungle_logs.contains(lv.get(p.rel(facing)))
        }
        _ => true,
    }
}

/// `RuleTest`, for ore targets.
#[derive(Debug, Clone)]
pub enum RuleTest {
    /// `always_true`.
    Always,
    /// `tag_match`.
    Tag(BlockSet),
    /// `block_match`.
    Block(u16),
    /// `blockstate_match`.
    State(BlockStateId),
    /// `random_block_match`.
    RandomBlock(u16, f32),
    /// `random_blockstate_match`.
    RandomState(BlockStateId, f32),
}

impl RuleTest {
    /// Parse.
    pub fn parse(j: &Json, tags: &Tags) -> Result<Self, BuildError> {
        let block = |k: &str| -> Result<u16, BuildError> {
            j.str_of(k)
                .and_then(blocks::block_id_of)
                .ok_or_else(|| BuildError::new(format!("rule test: bad `{k}`")))
        };
        let st = |k: &str| -> Result<BlockStateId, BuildError> {
            j.get(k)
                .and_then(parse_block_state)
                .ok_or_else(|| BuildError::new(format!("rule test: bad `{k}`")))
        };
        Ok(
            match j
                .str_of("predicate_type")
                .unwrap_or("")
                .trim_start_matches("minecraft:")
            {
                "always_true" => RuleTest::Always,
                "tag_match" => RuleTest::Tag(tags.block_tag(j.str_of("tag").unwrap_or(""))),
                "block_match" => RuleTest::Block(block("block")?),
                "blockstate_match" => RuleTest::State(st("block_state")?),
                "random_block_match" => {
                    RuleTest::RandomBlock(block("block")?, j.f64_or("probability", 1.0) as f32)
                }
                "random_blockstate_match" => {
                    RuleTest::RandomState(st("block_state")?, j.f64_or("probability", 1.0) as f32)
                }
                other => return Err(BuildError::new(format!("unknown rule test `{other}`"))),
            },
        )
    }

    /// `test(state, random)`.
    pub fn test(&self, s: BlockStateId, r: &mut WorldgenRandom) -> bool {
        match self {
            RuleTest::Always => true,
            RuleTest::Tag(t) => t.contains(s),
            RuleTest::Block(b) => blockinfo::block_of(s) == *b,
            RuleTest::State(st) => s == *st,
            RuleTest::RandomBlock(b, p) => blockinfo::block_of(s) == *b && r.next_float() < *p,
            RuleTest::RandomState(st, p) => s == *st && r.next_float() < *p,
        }
    }
}

/// `BlockPredicate`.
#[derive(Debug, Clone)]
pub enum BlockPredicate {
    /// `true`.
    True,
    /// `matching_blocks`.
    Blocks(Vec<i32>, BlockSet),
    /// `matching_block_tag`.
    Tag(Vec<i32>, BlockSet),
    /// `matching_fluids` (water / lava / flowing variants).
    Fluids(Vec<i32>, bool, bool),
    /// `solid`.
    Solid(Vec<i32>),
    /// `replaceable`.
    Replaceable(Vec<i32>),
    /// `would_survive`.
    WouldSurvive(Vec<i32>, BlockStateId),
    /// `has_sturdy_face`.
    Sturdy(Vec<i32>),
    /// `inside_world_bounds`.
    InsideWorld(Vec<i32>),
    /// `unobstructed`: no entities in worldgen, so always true.
    Unobstructed,
    /// `not`.
    Not(Box<BlockPredicate>),
    /// `all_of`.
    All(Vec<BlockPredicate>),
    /// `any_of`.
    Any(Vec<BlockPredicate>),
}

fn offset_of(j: &Json) -> Vec<i32> {
    match j.get("offset").and_then(Json::as_arr) {
        Some(a) if a.len() == 3 => a.iter().map(|v| v.as_f64().unwrap_or(0.0) as i32).collect(),
        _ => vec![0, 0, 0],
    }
}

impl BlockPredicate {
    /// Parse.
    pub fn parse(j: &Json, tags: &Tags) -> Result<Self, BuildError> {
        let off = offset_of(j);
        Ok(
            match j
                .str_of("type")
                .unwrap_or("")
                .trim_start_matches("minecraft:")
            {
                "true" => BlockPredicate::True,
                "matching_blocks" => BlockPredicate::Blocks(
                    off,
                    tags.holder_set(j.get("blocks").unwrap_or(&Json::Null)),
                ),
                "matching_block_tag" => {
                    BlockPredicate::Tag(off, tags.block_tag(j.str_of("tag").unwrap_or("")))
                }
                "matching_fluids" => {
                    let names: Vec<String> = match j.get("fluids") {
                        Some(Json::Str(s)) => vec![s.clone()],
                        Some(Json::Arr(a)) => a
                            .iter()
                            .filter_map(Json::as_str)
                            .map(str::to_string)
                            .collect(),
                        _ => Vec::new(),
                    };
                    let water = names.iter().any(|n| n.contains("water"));
                    let lava = names.iter().any(|n| n.contains("lava"));
                    BlockPredicate::Fluids(off, water, lava)
                }
                "solid" => BlockPredicate::Solid(off),
                "replaceable" => BlockPredicate::Replaceable(off),
                "would_survive" => BlockPredicate::WouldSurvive(
                    off,
                    j.get("state")
                        .and_then(parse_block_state)
                        .ok_or_else(|| BuildError::new("would_survive: bad state"))?,
                ),
                "has_sturdy_face" => BlockPredicate::Sturdy(off),
                "inside_world_bounds" => BlockPredicate::InsideWorld(off),
                "unobstructed" => BlockPredicate::Unobstructed,
                "not" => BlockPredicate::Not(Box::new(BlockPredicate::parse(
                    j.get("predicate")
                        .ok_or_else(|| BuildError::new("not: no predicate"))?,
                    tags,
                )?)),
                t @ ("all_of" | "any_of") => {
                    let mut v = Vec::new();
                    for p in j.get("predicates").and_then(Json::as_arr).unwrap_or(&[]) {
                        v.push(BlockPredicate::parse(p, tags)?);
                    }
                    if t == "all_of" {
                        BlockPredicate::All(v)
                    } else {
                        BlockPredicate::Any(v)
                    }
                }
                other => {
                    return Err(BuildError::new(format!(
                        "unknown block predicate `{other}`"
                    )))
                }
            },
        )
    }

    /// `test(level, pos)`.
    pub fn test(&self, lv: &Level, t: &CommonTags, p: Pos) -> bool {
        let at = |o: &[i32]| p.offset(o[0], o[1], o[2]);
        match self {
            BlockPredicate::True | BlockPredicate::Unobstructed => true,
            BlockPredicate::Blocks(o, set) | BlockPredicate::Tag(o, set) => {
                set.contains(lv.get(at(o)))
            }
            BlockPredicate::Fluids(o, water, lava) => {
                let s = lv.get(at(o));
                (*water && blockinfo::has_water(s)) || (*lava && blockinfo::is_lava(s))
            }
            BlockPredicate::Solid(o) => legacy_solid(lv.get(at(o))),
            BlockPredicate::Replaceable(o) => t.replaceable.contains(lv.get(at(o))),
            BlockPredicate::WouldSurvive(o, s) => can_survive(lv, t, *s, at(o)),
            BlockPredicate::Sturdy(o) => sturdy(lv.get(at(o))),
            BlockPredicate::InsideWorld(o) => !lv.outside_height(at(o).y),
            BlockPredicate::Not(inner) => !inner.test(lv, t, p),
            BlockPredicate::All(v) => v.iter().all(|x| x.test(lv, t, p)),
            BlockPredicate::Any(v) => v.iter().any(|x| x.test(lv, t, p)),
        }
    }
}

/// `BlockStateProvider`.
#[derive(Debug, Clone)]
pub enum StateProvider {
    /// `simple_state_provider`.
    Simple(BlockStateId),
    /// `weighted_state_provider`.
    Weighted(Vec<(BlockStateId, i32)>, i32),
    /// `randomized_int_state_provider`.
    RandomizedInt(Box<StateProvider>, String, IntProvider),
    /// `rotated_block_provider`.
    Rotated(BlockStateId),
    /// `noise_provider`.
    Noise {
        noise: Arc<NormalNoise>,
        scale: f64,
        states: Vec<BlockStateId>,
    },
    /// `noise_threshold_provider`.
    NoiseThreshold {
        noise: Arc<NormalNoise>,
        scale: f64,
        threshold: f32,
        high_chance: f32,
        default: BlockStateId,
        low: Vec<BlockStateId>,
        high: Vec<BlockStateId>,
    },
    /// `dual_noise_provider`.
    DualNoise {
        noise: Arc<NormalNoise>,
        scale: f64,
        slow: Arc<NormalNoise>,
        slow_scale: f32,
        variety: (i32, i32),
        states: Vec<BlockStateId>,
    },
}

fn state_list(j: Option<&Json>) -> Vec<BlockStateId> {
    j.and_then(Json::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter_map(parse_block_state)
        .collect()
}

impl StateProvider {
    /// Parse. `noises` builds the legacy-seeded noises the noise-based
    /// providers carry.
    pub fn parse(j: &Json, noises: &LegacyNoises) -> Result<Self, BuildError> {
        let ty = j
            .str_of("type")
            .unwrap_or("")
            .trim_start_matches("minecraft:");
        Ok(match ty {
            "simple_state_provider" => StateProvider::Simple(
                j.get("state")
                    .and_then(parse_block_state)
                    .ok_or_else(|| BuildError::new("simple_state_provider: bad state"))?,
            ),
            "weighted_state_provider" => {
                let mut v = Vec::new();
                let mut total = 0;
                for e in j.get("entries").and_then(Json::as_arr).unwrap_or(&[]) {
                    if let Some(s) = e.get("data").and_then(parse_block_state) {
                        let w = e.i32_or("weight", 1);
                        total += w;
                        v.push((s, w));
                    }
                }
                if v.is_empty() {
                    return Err(BuildError::new(
                        "weighted_state_provider: no usable entries",
                    ));
                }
                StateProvider::Weighted(v, total)
            }
            "randomized_int_state_provider" => StateProvider::RandomizedInt(
                Box::new(StateProvider::parse(
                    j.get("source")
                        .ok_or_else(|| BuildError::new("randomized_int: no source"))?,
                    noises,
                )?),
                j.str_of("property").unwrap_or("").to_string(),
                IntProvider::parse(j.get("values").unwrap_or(&Json::Null))?,
            ),
            "rotated_block_provider" => StateProvider::Rotated(
                j.get("state")
                    .and_then(parse_block_state)
                    .ok_or_else(|| BuildError::new("rotated_block_provider: bad state"))?,
            ),
            "noise_provider" | "noise_threshold_provider" | "dual_noise_provider" => {
                let seed = j.get("seed").and_then(Json::as_f64).unwrap_or(0.0) as i64;
                let noise = noises.get(seed, j.get("noise").unwrap_or(&Json::Null))?;
                let scale = j.f64_or("scale", 1.0) as f32 as f64;
                match ty {
                    "noise_provider" => StateProvider::Noise {
                        noise,
                        scale,
                        states: state_list(j.get("states")),
                    },
                    "noise_threshold_provider" => StateProvider::NoiseThreshold {
                        noise,
                        scale,
                        threshold: j.f64_or("threshold", 0.0) as f32,
                        high_chance: j.f64_or("high_chance", 0.0) as f32,
                        default: j
                            .get("default_state")
                            .and_then(parse_block_state)
                            .ok_or_else(|| {
                                BuildError::new("noise_threshold_provider: bad default")
                            })?,
                        low: state_list(j.get("low_states")),
                        high: state_list(j.get("high_states")),
                    },
                    _ => {
                        let v = j.get("variety").and_then(Json::as_arr).unwrap_or(&[]);
                        let variety = (
                            v.first().and_then(Json::as_f64).unwrap_or(1.0) as i32,
                            v.get(1).and_then(Json::as_f64).unwrap_or(1.0) as i32,
                        );
                        StateProvider::DualNoise {
                            noise,
                            scale,
                            slow: noises
                                .get(seed + 1, j.get("slow_noise").unwrap_or(&Json::Null))?,
                            slow_scale: j.f64_or("slow_scale", 1.0) as f32,
                            variety,
                            states: state_list(j.get("states")),
                        }
                    }
                }
            }
            other => return Err(BuildError::new(format!("unknown state provider `{other}`"))),
        })
    }

    /// `getState(random, pos)`.
    pub fn get(&self, r: &mut WorldgenRandom, p: Pos) -> BlockStateId {
        match self {
            StateProvider::Simple(s) => *s,
            StateProvider::Weighted(v, total) => {
                let mut i = r.next_int_bounded(*total);
                for (s, w) in v {
                    if i < *w {
                        return *s;
                    }
                    i -= w;
                }
                v[0].0
            }
            StateProvider::RandomizedInt(src, prop_name, values) => {
                let s = src.get(r, p);
                if prop(s, prop_name).is_none() {
                    return s;
                }
                let v = values.sample(r);
                blockinfo::with_prop(s, prop_name, &v.to_string())
            }
            StateProvider::Rotated(s) => {
                let axis = ["x", "y", "z"][r.next_int_bounded(3) as usize];
                blockinfo::with_prop(*s, "axis", axis)
            }
            StateProvider::Noise {
                noise,
                scale,
                states,
            } => pick(states, noise_at(noise, p, *scale)),
            StateProvider::NoiseThreshold {
                noise,
                scale,
                threshold,
                high_chance,
                default,
                low,
                high,
            } => {
                let v = noise_at(noise, p, *scale);
                if v < *threshold as f64 {
                    low[r.next_int_bounded(low.len() as i32) as usize]
                } else if r.next_float() < *high_chance {
                    high[r.next_int_bounded(high.len() as i32) as usize]
                } else {
                    *default
                }
            }
            StateProvider::DualNoise {
                noise,
                scale,
                slow,
                slow_scale,
                variety,
                states,
            } => {
                let slow_at = |q: Pos| {
                    slow.get_value(
                        (q.x as f32 * slow_scale) as f64,
                        (q.y as f32 * slow_scale) as f64,
                        (q.z as f32 * slow_scale) as f64,
                    )
                };
                let n = super::super::mth::clamped_map(
                    slow_at(p),
                    -1.0,
                    1.0,
                    variety.0 as f64,
                    (variety.1 + 1) as f64,
                ) as i32;
                let mut list = Vec::with_capacity(n.max(0) as usize);
                for i in 0..n {
                    list.push(pick(states, slow_at(p.offset(i * 54545, 0, i * 34234))));
                }
                if list.is_empty() {
                    return states[0];
                }
                pick(&list, noise_at(noise, p, *scale))
            }
        }
    }
}

fn noise_at(n: &NormalNoise, p: Pos, scale: f64) -> f64 {
    n.get_value(p.x as f64 * scale, p.y as f64 * scale, p.z as f64 * scale)
}

fn pick(states: &[BlockStateId], v: f64) -> BlockStateId {
    let t = ((1.0 + v) / 2.0).clamp(0.0, 0.9999);
    states[(t * states.len() as f64) as usize]
}

/// Builds the legacy-LCG-seeded `NormalNoise`s noise-based state providers
/// carry (`NormalNoise.create(new WorldgenRandom(new LegacyRandomSource(seed)), …)`).
pub struct LegacyNoises<'a> {
    pub(crate) _noises: &'a NoiseRegistry,
}

impl LegacyNoises<'_> {
    /// The noise for a seed and a `{firstOctave, amplitudes}` object.
    pub fn get(&self, seed: i64, params: &Json) -> Result<Arc<NormalNoise>, BuildError> {
        let first = params.i32_or("firstOctave", 0);
        let amps: Vec<f64> = params
            .get("amplitudes")
            .and_then(Json::as_arr)
            .unwrap_or(&[])
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0))
            .collect();
        if amps.is_empty() {
            return Err(BuildError::new("noise parameters: no amplitudes"));
        }
        Ok(Arc::new(NormalNoise::create_legacy(seed, first, &amps)))
    }
}
