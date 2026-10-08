//! Configured features: parsing and placement, ported from the game's
//! `Feature` subclasses. Each `place_*` draws from the random source in the
//! same order as its Java counterpart.

use std::sync::{Arc, OnceLock};

use aether_world::BlockStateId;

use super::super::blockinfo::{self, prop};
use super::super::density::BuildError;
use super::super::json::Json;
use super::super::providers::IntProvider;
use super::super::rng::Rng;
use super::super::surface::parse_block_state;
use super::super::tags::BlockSet;
use super::blocks::{can_survive, is_water_source, legacy_solid, sturdy, BlockPredicate, RuleTest, StateProvider};
use super::level::{Dir, Heightmap, Pos};
use super::tree::{self, FallenTree, TreeConfig};
use super::{place_placed, Ctx, Loader, Placed};

/// One ore target.
#[derive(Debug)]
pub struct OreTarget {
    test: RuleTest,
    state: BlockStateId,
}

/// A configured feature.
#[derive(Debug)]
pub enum Configured {
    /// `simple_block`.
    SimpleBlock { provider: StateProvider, schedule_tick: bool },
    /// `random_patch`, `flower`, `no_bonemeal_flower`.
    RandomPatch { tries: i32, xz: i32, y: i32, feature: Arc<Placed> },
    /// `ore`.
    Ore { targets: Vec<OreTarget>, size: i32, discard: f32 },
    /// `scattered_ore`.
    ScatteredOre { targets: Vec<OreTarget>, size: i32, discard: f32 },
    /// `random_selector`.
    RandomSelector { features: Vec<(Arc<Placed>, f32)>, default: Arc<Placed> },
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
    Lake { fluid: StateProvider, barrier: StateProvider },
    /// `bamboo`.
    Bamboo(f32),
    /// `forest_rock` (`BlockBlobFeature`).
    BlockBlob(BlockStateId),
    /// `ice_spike`.
    IceSpike,
    /// `blue_ice`.
    BlueIce,
    /// `underwater_magma`.
    UnderwaterMagma { range: i32, radius: i32, probability: f32 },
    /// `multiface_growth` (no spreading).
    MultifaceGrowth { block: BlockStateId, search_range: i32, floor: bool, ceiling: bool, wall: bool, spread: f32, on: BlockSet },
    /// `huge_red_mushroom` / `huge_brown_mushroom`.
    HugeMushroom { red: bool, cap: StateProvider, stem: StateProvider, radius: i32 },
    /// `tree`.
    Tree(Box<TreeConfig>),
    /// `fallen_tree`.
    FallenTree(Box<FallenTree>),
    /// A type this port does not place.
    Unsupported(String),
}

fn get<'j>(j: &'j Json, k: &str) -> Result<&'j Json, BuildError> {
    j.get(k).ok_or_else(|| BuildError::new(format!("missing `{k}`")))
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
    let t = j.str_of("type").unwrap_or("").trim_start_matches("minecraft:").to_string();
    let empty = Json::Obj(Default::default());
    let c = j.get("config").unwrap_or(&empty);
    let sp = |l: &Loader, k: &str| -> Result<StateProvider, BuildError> { StateProvider::parse(get(c, k)?, &l.noises) };
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
                features.push((l.placed(get(f, "feature")?)?, f.f64_or("chance", 0.0) as f32));
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
        "random_boolean_selector" => {
            Configured::RandomBoolean(l.placed(get(c, "feature_true")?)?, l.placed(get(c, "feature_false")?)?)
        }
        "block_column" => {
            let mut layers = Vec::new();
            for layer in c.get("layers").and_then(Json::as_arr).unwrap_or(&[]) {
                layers.push((IntProvider::parse(get(layer, "height")?)?, StateProvider::parse(get(layer, "provider")?, &l.noises)?));
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
                rules.push((l.predicate(get(r, "if_true")?)?, StateProvider::parse(get(r, "then")?, &l.noises)?));
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
                valid: l.tags.holder_set(c.get("valid_blocks").unwrap_or(&Json::Null)),
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
            on: l.tags.holder_set(c.get("can_be_placed_on").unwrap_or(&Json::Null)),
        },
        "huge_red_mushroom" | "huge_brown_mushroom" => Configured::HugeMushroom {
            red: t == "huge_red_mushroom",
            cap: sp(l, "cap_provider")?,
            stem: sp(l, "stem_provider")?,
            radius: c.i32_or("foliage_radius", 2),
        },
        "tree" => Configured::Tree(Box::new(tree::parse_tree(l, c)?)),
        "fallen_tree" => Configured::FallenTree(Box::new(tree::parse_fallen(l, c)?)),
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
        Configured::RandomPatch { tries, xz, y, feature } => {
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
        Configured::Ore { targets, size, discard } => ore(ctx, targets, *size, *discard, pos),
        Configured::ScatteredOre { targets, size, discard } => {
            let n = ctx.r.next_int_bounded(size + 1);
            for i in 0..n {
                let d = i.min(7);
                let mut axis = || ((ctx.r.next_float() - ctx.r.next_float()) * d as f32).round() as i32;
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
            let around = [pos.rel(Dir::West), pos.rel(Dir::East), pos.rel(Dir::North), pos.rel(Dir::South), pos.below(1)];
            let rocks = around.iter().filter(|p| valid.contains(lv.get(**p))).count() as i32;
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
        } => multiface(ctx, *block, *search_range, *floor, *ceiling, *wall, *spread, on, pos),
        Configured::HugeMushroom { red, cap, stem, radius } => huge_mushroom(ctx, *red, cap, stem, *radius, pos),
        Configured::Tree(cfg) => tree::place_tree(ctx, cfg, pos),
        Configured::FallenTree(cfg) => tree::place_fallen(ctx, cfg, pos),
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
                return ore_place(ctx, targets, size, discard, [x0, x1, z0, z1, y0, y1], min_x, min_y, min_z, w, h);
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
        let r = ((super::super::mth::sin((std::f32::consts::PI * t) as f64) + 1.0) as f64 * s + 1.0) / 2.0;
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
    let st = if tall { states().tall_seagrass } else { states().seagrass };
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
                ctx.lv.set(p, blockinfo::with_prop(st.kelp, "age", &age.to_string()));
                placed += 1;
            } else {
                ctx.lv.set(p, st.kelp_plant);
            }
        } else if i > 0 {
            let b = p.below(1);
            if can_survive(ctx.lv, ctx.tags(), st.kelp, b) && !is(ctx.lv.get(b.below(1)), "minecraft:kelp") {
                let age = ctx.r.next_int_bounded(4) + 20;
                ctx.lv.set(b, blockinfo::with_prop(st.kelp, "age", &age.to_string()));
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
    if !core.biomes.cold_enough_to_snow(biome, p.x, p.y, p.z, core.sea_level) {
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
    if !info.has_precipitation || !core.biomes.cold_enough_to_snow(biome, p.x, p.y, p.z, core.sea_level) {
        return false;
    }
    if ctx.lv.outside_height(p.y) {
        return false;
    }
    let s = ctx.lv.get(p);
    (blockinfo::is_air(s) || is(s, "minecraft:snow")) && can_survive(ctx.lv, ctx.tags(), states().snow, p)
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
                    if y >= 4 && matches!(blockinfo::name(s), "minecraft:water" | "minecraft:lava") {
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
                        ctx.lv.set(p, if y >= 4 { states().cave_air } else { fluid });
                    }
                }
            }
        }
    }
    let barrier = barrier_p.get(ctx.r, base);
    if !blockinfo::is_air(barrier) {
        let stone_cannot = ctx.d.block_tags.block_tag("minecraft:lava_pool_stone_cannot_replace");
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
        blockinfo::is_air(s) || ctx.d.tags.dirt.contains(s) || is(s, "minecraft:snow_block") || is(s, "minecraft:ice")
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
            if blockinfo::is_air(s) || is(s, "minecraft:water") || is(s, "minecraft:packed_ice") || is(s, "minecraft:ice") {
                if Dir::ALL.iter().any(|d| is(ctx.lv.get(q.rel(*d)), "minecraft:blue_ice")) {
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
    let shuffled = shuffle(ctx, &dirs);
    if try_multiface(ctx, block, spread, on, pos, &shuffled) {
        return true;
    }
    for d in shuffled.clone() {
        let others: Vec<Dir> = dirs.iter().copied().filter(|x| *x != d.opposite()).collect();
        let others = shuffle(ctx, &others);
        let mut p = pos;
        for _ in 0..search {
            p = p.rel(d);
            let s = ctx.lv.get(p);
            if !air_or_water(s) && blockinfo::block_of(s) != blockinfo::block_of(block) {
                break;
            }
            if try_multiface(ctx, block, spread, on, p, &others) {
                return true;
            }
        }
    }
    false
}

fn try_multiface(ctx: &mut Ctx, block: BlockStateId, spread: f32, on: &BlockSet, p: Pos, dirs: &[Dir]) -> bool {
    for d in dirs {
        if on.contains(ctx.lv.get(p.rel(*d))) {
            let here = ctx.lv.get(p);
            let mut s = if blockinfo::block_of(here) == blockinfo::block_of(block) { here } else { block };
            s = blockinfo::with_prop(s, d.name(), "true");
            let wet = is(here, "minecraft:water");
            s = blockinfo::with_prop(s, "waterlogged", if wet { "true" } else { "false" });
            ctx.lv.set(p, s);
            // Spreading to neighbouring faces is not ported; the draw is.
            let _ = ctx.r.next_float() < spread;
            return true;
        }
    }
    false
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

fn huge_mushroom(ctx: &mut Ctx, red: bool, cap: &StateProvider, stem: &StateProvider, radius: i32, pos: Pos) -> bool {
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
    let by_mush = ctx.d.block_tags.block_tag("minecraft:replaceable_by_mushrooms");
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
