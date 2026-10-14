//! Pointed dripstone: `dripstone_cluster`, `large_dripstone` and
//! `pointed_dripstone`, ported from the game's three feature classes and the
//! `DripstoneUtils` column/height maths they share. Every random draw is made
//! in the game's order (including the short-circuits in `&&`).

use std::sync::OnceLock;

use aether_world::BlockStateId;

use super::super::blockinfo;
use super::super::density::BuildError;
use super::super::json::Json;
use super::super::mth;
use super::super::providers::{mth_next_int, FloatProvider, IntProvider};
use super::super::rng::{Rng, WorldgenRandom};
use super::blocks::CommonTags;
use super::level::{Dir, Heightmap, Pos};
use super::Ctx;

// --- configs ---------------------------------------------------------------

/// `DripstoneClusterConfiguration`.
#[derive(Debug)]
pub struct DripstoneCluster {
    pub chance_of_dripstone_column_at_max_distance_from_center: f32,
    pub density: FloatProvider,
    pub dripstone_block_layer_thickness: IntProvider,
    pub floor_to_ceiling_search_range: i32,
    pub height: IntProvider,
    pub height_deviation: f32,
    pub max_distance_from_center_affecting_height_bias: i32,
    pub max_distance_from_edge_affecting_chance_of_dripstone_column: i32,
    pub max_stalagmite_stalactite_height_diff: i32,
    pub radius: IntProvider,
    pub wetness: FloatProvider,
}

impl DripstoneCluster {
    /// Parse.
    pub fn parse(j: &Json) -> Result<Self, BuildError> {
        Ok(Self {
            chance_of_dripstone_column_at_max_distance_from_center: j.f64_or(
                "chance_of_dripstone_column_at_max_distance_from_center",
                0.0,
            ) as f32,
            density: FloatProvider::parse(
                j.get("density")
                    .ok_or_else(|| BuildError::new("dripstone_cluster: no density"))?,
            )?,
            dripstone_block_layer_thickness: IntProvider::parse(
                j.get("dripstone_block_layer_thickness")
                    .ok_or_else(|| BuildError::new("dripstone_cluster: no layer thickness"))?,
            )?,
            floor_to_ceiling_search_range: j.i32_or("floor_to_ceiling_search_range", 0),
            height: IntProvider::parse(
                j.get("height")
                    .ok_or_else(|| BuildError::new("dripstone_cluster: no height"))?,
            )?,
            height_deviation: j.f64_or("height_deviation", 0.0) as f32,
            max_distance_from_center_affecting_height_bias: j
                .i32_or("max_distance_from_center_affecting_height_bias", 0),
            max_distance_from_edge_affecting_chance_of_dripstone_column: j.i32_or(
                "max_distance_from_edge_affecting_chance_of_dripstone_column",
                0,
            ),
            max_stalagmite_stalactite_height_diff: j
                .i32_or("max_stalagmite_stalactite_height_diff", 0),
            radius: IntProvider::parse(
                j.get("radius")
                    .ok_or_else(|| BuildError::new("dripstone_cluster: no radius"))?,
            )?,
            wetness: FloatProvider::parse(
                j.get("wetness")
                    .ok_or_else(|| BuildError::new("dripstone_cluster: no wetness"))?,
            )?,
        })
    }
}

/// `LargeDripstoneConfiguration`.
#[derive(Debug)]
pub struct LargeDripstone {
    pub column_radius: IntProvider,
    pub floor_to_ceiling_search_range: i32,
    pub height_scale: FloatProvider,
    pub max_column_radius_to_cave_height_ratio: f32,
    pub min_bluntness_for_wind: f32,
    pub min_radius_for_wind: i32,
    pub stalactite_bluntness: FloatProvider,
    pub stalagmite_bluntness: FloatProvider,
    pub wind_speed: FloatProvider,
}

impl LargeDripstone {
    /// Parse.
    pub fn parse(j: &Json) -> Result<Self, BuildError> {
        Ok(Self {
            column_radius: IntProvider::parse(
                j.get("column_radius")
                    .ok_or_else(|| BuildError::new("large_dripstone: no column_radius"))?,
            )?,
            floor_to_ceiling_search_range: j.i32_or("floor_to_ceiling_search_range", 0),
            height_scale: FloatProvider::parse(
                j.get("height_scale")
                    .ok_or_else(|| BuildError::new("large_dripstone: no height_scale"))?,
            )?,
            max_column_radius_to_cave_height_ratio: j
                .f64_or("max_column_radius_to_cave_height_ratio", 0.0)
                as f32,
            min_bluntness_for_wind: j.f64_or("min_bluntness_for_wind", 0.0) as f32,
            min_radius_for_wind: j.i32_or("min_radius_for_wind", 0),
            stalactite_bluntness: FloatProvider::parse(
                j.get("stalactite_bluntness")
                    .ok_or_else(|| BuildError::new("large_dripstone: no stalactite_bluntness"))?,
            )?,
            stalagmite_bluntness: FloatProvider::parse(
                j.get("stalagmite_bluntness")
                    .ok_or_else(|| BuildError::new("large_dripstone: no stalagmite_bluntness"))?,
            )?,
            wind_speed: FloatProvider::parse(
                j.get("wind_speed")
                    .ok_or_else(|| BuildError::new("large_dripstone: no wind_speed"))?,
            )?,
        })
    }
}

/// `PointedDripstoneConfiguration`.
#[derive(Debug)]
pub struct PointedDripstone {
    pub chance_of_directional_spread: f32,
    pub chance_of_spread_radius2: f32,
    pub chance_of_spread_radius3: f32,
    pub chance_of_taller_dripstone: f32,
}

impl PointedDripstone {
    /// Parse.
    pub fn parse(j: &Json) -> Result<Self, BuildError> {
        Ok(Self {
            chance_of_directional_spread: j.f64_or("chance_of_directional_spread", 1.0) as f32,
            chance_of_spread_radius2: j.f64_or("chance_of_spread_radius2", 1.0) as f32,
            chance_of_spread_radius3: j.f64_or("chance_of_spread_radius3", 1.0) as f32,
            chance_of_taller_dripstone: j.f64_or("chance_of_taller_dripstone", 0.0) as f32,
        })
    }
}

// --- states ----------------------------------------------------------------

fn is(s: BlockStateId, n: &str) -> bool {
    blockinfo::name(s) == n
}

fn is_water(s: BlockStateId) -> bool {
    is(s, "minecraft:water")
}

fn is_empty_or_water(s: BlockStateId) -> bool {
    blockinfo::is_air(s) || is_water(s)
}

fn is_neither_empty_nor_water(s: BlockStateId) -> bool {
    !blockinfo::is_air(s) && !is_water(s)
}

fn is_empty_or_water_or_lava(s: BlockStateId) -> bool {
    blockinfo::is_air(s) || is_water(s) || blockinfo::is_lava(s)
}

fn is_dripstone_base(s: BlockStateId, t: &CommonTags) -> bool {
    is(s, "minecraft:dripstone_block") || t.dripstone_replaceable.contains(s)
}

fn dripstone_block() -> BlockStateId {
    static S: OnceLock<BlockStateId> = OnceLock::new();
    *S.get_or_init(|| blockinfo::parse_state("minecraft:dripstone_block").expect("dripstone_block"))
}

fn water() -> BlockStateId {
    static S: OnceLock<BlockStateId> = OnceLock::new();
    *S.get_or_init(|| blockinfo::parse_state("minecraft:water[level=0]").expect("water"))
}

fn pointed_dripstone_state(thickness: &str, dir: Dir, waterlogged: bool) -> BlockStateId {
    static S: OnceLock<BlockStateId> = OnceLock::new();
    let base = *S.get_or_init(|| {
        blockinfo::parse_state("minecraft:pointed_dripstone").expect("pointed_dripstone")
    });
    let s = blockinfo::with_prop(base, "thickness", thickness);
    let s = blockinfo::with_prop(s, "vertical_direction", dir.name());
    blockinfo::with_prop(s, "waterlogged", if waterlogged { "true" } else { "false" })
}

// --- DripstoneUtils --------------------------------------------------------

/// `DripstoneUtils.getDripstoneHeight`.
fn dripstone_height_curve(d: f64, e: f64, f: f64, g: f64) -> f64 {
    let d = d.max(g);
    let i = d / e * 0.384;
    let j = 0.75 * i.powf(1.3333333333333333);
    let k = i.powf(0.6666666666666666);
    let l = 0.3333333333333333 * i.ln();
    let m = (f * (j - k - l)).max(0.0);
    m / 0.384 * e
}

/// `DripstoneUtils.isCircleMostlyEmbeddedInStone`.
fn is_circle_mostly_embedded_in_stone(ctx: &Ctx, center: Pos, radius: i32) -> bool {
    if is_empty_or_water_or_lava(ctx.lv.get(center)) {
        return false;
    }
    let step = 6.0f32 / radius as f32;
    let end = (std::f64::consts::PI * 2.0) as f32;
    let mut h = 0.0f32;
    while h < end {
        let j = (mth::cos(h as f64) * radius as f32) as i32;
        let k = (mth::sin(h as f64) * radius as f32) as i32;
        if is_empty_or_water_or_lava(ctx.lv.get(center.offset(j, 0, k))) {
            return false;
        }
        h += step;
    }
    true
}

/// `DripstoneUtils.buildBaseToTipColumn` + `growPointedDripstone`.
fn grow_pointed_dripstone(ctx: &mut Ctx, p: Pos, dir: Dir, n: i32, merge: bool) {
    let base = is_dripstone_base(ctx.lv.get(p.rel(dir.opposite())), ctx.tags());
    if !base {
        return;
    }
    let mut cur = p;
    let mut emit = |thickness: &str| {
        let wl = blockinfo::has_water(ctx.lv.get(cur));
        ctx.lv.set(cur, pointed_dripstone_state(thickness, dir, wl));
        cur = cur.rel(dir);
    };
    if n >= 3 {
        emit("base");
        for _ in 0..n - 3 {
            emit("middle");
        }
    }
    if n >= 2 {
        emit("frustum");
    }
    if n >= 1 {
        emit(if merge { "tip_merge" } else { "tip" });
    }
}

/// `DripstoneUtils.placeDripstoneBlockIfPossible`.
fn place_dripstone_block_if_possible(ctx: &mut Ctx, p: Pos) -> bool {
    let s = ctx.lv.get(p);
    if ctx.tags().dripstone_replaceable.contains(s) {
        let b = dripstone_block();
        ctx.lv.set(p, b);
        true
    } else {
        false
    }
}

fn replace_blocks_with_dripstone_blocks(ctx: &mut Ctx, p: Pos, n: i32, dir: Dir) {
    let mut cur = p;
    for _ in 0..n {
        if !place_dripstone_block_if_possible(ctx, cur) {
            return;
        }
        cur = cur.rel(dir);
    }
}

fn random_dir(ctx: &mut Ctx) -> Dir {
    Dir::ALL[ctx.r.next_int_bounded(6) as usize]
}

// --- Column ----------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Column {
    ceiling: Option<i32>,
    floor: Option<i32>,
    height: Option<i32>,
}

/// `Column.create(floor, ceiling)`.
fn column_create(floor: Option<i32>, ceiling: Option<i32>) -> Column {
    match (floor, ceiling) {
        (Some(f), Some(c)) => Column {
            floor: Some(f),
            ceiling: Some(c),
            height: Some(c - f - 1),
        },
        (Some(f), None) => Column {
            floor: Some(f),
            ceiling: None,
            height: None,
        },
        (None, Some(c)) => Column {
            floor: None,
            ceiling: Some(c),
            height: None,
        },
        (None, None) => Column {
            floor: None,
            ceiling: None,
            height: None,
        },
    }
}

impl Column {
    /// `Column.withFloor`.
    fn with_floor(self, floor: Option<i32>) -> Column {
        column_create(floor, self.ceiling)
    }
}

fn scan_dir(
    ctx: &Ctx,
    origin: Pos,
    range: i32,
    empty: &dyn Fn(BlockStateId) -> bool,
    solid: &dyn Fn(BlockStateId) -> bool,
    dir: Dir,
) -> Option<i32> {
    let mut p = Pos::new(origin.x, origin.y, origin.z);
    let mut k = 1;
    while k < range && empty(ctx.lv.get(p)) {
        p = p.rel(dir);
        k += 1;
    }
    if solid(ctx.lv.get(p)) {
        Some(p.y)
    } else {
        None
    }
}

/// `Column.scan`.
fn column_scan(
    ctx: &Ctx,
    origin: Pos,
    range: i32,
    empty: &dyn Fn(BlockStateId) -> bool,
    solid: &dyn Fn(BlockStateId) -> bool,
) -> Option<Column> {
    if !empty(ctx.lv.get(origin)) {
        return None;
    }
    let ceiling = scan_dir(ctx, origin, range, empty, solid, Dir::Up);
    let floor = scan_dir(ctx, origin, range, empty, solid, Dir::Down);
    Some(column_create(floor, ceiling))
}

// --- pointed_dripstone -----------------------------------------------------

fn tip_direction(ctx: &mut Ctx, origin: Pos) -> Option<Dir> {
    let above = is_dripstone_base(ctx.lv.get(origin.above(1)), ctx.tags());
    let below = is_dripstone_base(ctx.lv.get(origin.below(1)), ctx.tags());
    if above && below {
        Some(if ctx.r.next_bool() {
            Dir::Down
        } else {
            Dir::Up
        })
    } else if above {
        Some(Dir::Down)
    } else if below {
        Some(Dir::Up)
    } else {
        None
    }
}

fn create_patch_of_dripstone_blocks(ctx: &mut Ctx, c: &PointedDripstone, p: Pos) {
    place_dripstone_block_if_possible(ctx, p);
    for dir in Dir::HORIZONTAL {
        if ctx.r.next_float() > c.chance_of_directional_spread {
            continue;
        }
        let p2 = p.rel(dir);
        place_dripstone_block_if_possible(ctx, p2);
        if ctx.r.next_float() > c.chance_of_spread_radius2 {
            continue;
        }
        let p3 = p2.rel(random_dir(ctx));
        place_dripstone_block_if_possible(ctx, p3);
        if ctx.r.next_float() > c.chance_of_spread_radius3 {
            continue;
        }
        let p4 = p3.rel(random_dir(ctx));
        place_dripstone_block_if_possible(ctx, p4);
    }
}

/// `PointedDripstoneFeature.place`.
pub(crate) fn pointed_dripstone(ctx: &mut Ctx, c: &PointedDripstone, origin: Pos) -> bool {
    let Some(dir) = tip_direction(ctx, origin) else {
        return false;
    };
    let p2 = origin.rel(dir.opposite());
    create_patch_of_dripstone_blocks(ctx, c, p2);
    let taller = ctx.r.next_float() < c.chance_of_taller_dripstone
        && is_empty_or_water(ctx.lv.get(origin.rel(dir)));
    let n = if taller { 2 } else { 1 };
    grow_pointed_dripstone(ctx, origin, dir, n, false);
    true
}

// --- dripstone_cluster -----------------------------------------------------

fn can_be_adjacent_to_water(ctx: &Ctx, p: Pos) -> bool {
    let s = ctx.lv.get(p);
    ctx.tags().base_stone_overworld.contains(s) || blockinfo::has_water(s)
}

fn can_place_pool(ctx: &Ctx, p: Pos) -> bool {
    let s = ctx.lv.get(p);
    if is_water(s) || is(s, "minecraft:dripstone_block") || is(s, "minecraft:pointed_dripstone") {
        return false;
    }
    if blockinfo::has_water(ctx.lv.get(p.above(1))) {
        return false;
    }
    for dir in Dir::HORIZONTAL {
        if !can_be_adjacent_to_water(ctx, p.rel(dir)) {
            return false;
        }
    }
    can_be_adjacent_to_water(ctx, p.below(1))
}

fn cluster_height(ctx: &mut Ctx, i: i32, j: i32, f: f32, k: i32, c: &DripstoneCluster) -> i32 {
    if ctx.r.next_float() > f {
        return 0;
    }
    let l = i.abs() + j.abs();
    let bias = mth::clamped_map(
        l as f64,
        0.0,
        c.max_distance_from_center_affecting_height_bias as f64,
        k as f64 / 2.0,
        0.0,
    ) as f32;
    (bias + ctx.r.next_gaussian() as f32 * c.height_deviation).clamp(0.0, k as f32) as i32
}

fn chance_of_stalagmite_or_stalactite(i: i32, j: i32, k: i32, l: i32, c: &DripstoneCluster) -> f64 {
    let m = i - k.abs();
    let n = j - l.abs();
    let o = m.min(n);
    mth::clamped_map_f32(
        o as f32,
        0.0,
        c.max_distance_from_edge_affecting_chance_of_dripstone_column as f32,
        c.chance_of_dripstone_column_at_max_distance_from_center,
        1.0,
    ) as f64
}

#[allow(clippy::too_many_arguments)]
fn place_column(
    ctx: &mut Ctx,
    c: &DripstoneCluster,
    p: Pos,
    i: i32,
    j: i32,
    f: f32,
    d: f64,
    k: i32,
    g: f32,
) {
    let mut column = match column_scan(
        ctx,
        p,
        c.floor_to_ceiling_search_range,
        &is_empty_or_water,
        &is_neither_empty_nor_water,
    ) {
        Some(col) => col,
        None => return,
    };
    let ceiling = column.ceiling;
    let floor = column.floor;
    if ceiling.is_none() && floor.is_none() {
        return;
    }

    let wet = ctx.r.next_float() < f;
    if wet {
        if let Some(fl) = floor {
            if can_place_pool(ctx, p.at_y(fl)) {
                column = column.with_floor(Some(fl - 1));
                let w = water();
                ctx.lv.set(p.at_y(fl), w);
            }
        }
    }
    let floor2 = column.floor;

    let bl2 = ctx.r.next_double() < d;
    let o = if bl2 {
        match ceiling {
            Some(cel) if !blockinfo::is_lava(ctx.lv.get(p.at_y(cel))) => {
                let m = c.dripstone_block_layer_thickness.sample(ctx.r);
                replace_blocks_with_dripstone_blocks(ctx, p.at_y(cel), m, Dir::Up);
                let n = match floor2 {
                    Some(fl) => k.min(cel - fl),
                    None => k,
                };
                cluster_height(ctx, i, j, g, n, c)
            }
            _ => 0,
        }
    } else {
        0
    };

    let bl3 = ctx.r.next_double() < d;
    let m = if bl3 {
        match floor2 {
            Some(fl) if !blockinfo::is_lava(ctx.lv.get(p.at_y(fl))) => {
                let thick = c.dripstone_block_layer_thickness.sample(ctx.r);
                replace_blocks_with_dripstone_blocks(ctx, p.at_y(fl), thick, Dir::Down);
                if ceiling.is_some() {
                    (o + mth_next_int(
                        ctx.r,
                        -c.max_stalagmite_stalactite_height_diff,
                        c.max_stalagmite_stalactite_height_diff,
                    ))
                    .max(0)
                } else {
                    cluster_height(ctx, i, j, g, k, c)
                }
            }
            _ => 0,
        }
    } else {
        0
    };

    let (p2, w) = match (ceiling, floor2) {
        (Some(r), Some(q)) if r - o <= q + m => {
            let s = (r - o).max(q + 1);
            let t = (q + m).min(r - 1);
            let u = mth_next_int(ctx.r, s, t + 1);
            let v = u - 1;
            (r - u, v - q)
        }
        _ => (o, m),
    };

    let merge = ctx.r.next_bool() && p2 > 0 && w > 0 && column.height == Some(p2 + w);
    if let Some(cel) = ceiling {
        grow_pointed_dripstone(ctx, p.at_y(cel - 1), Dir::Down, p2, merge);
    }
    if let Some(fl) = floor2 {
        grow_pointed_dripstone(ctx, p.at_y(fl + 1), Dir::Up, w, merge);
    }
}

/// `DripstoneClusterFeature.place`.
pub(crate) fn dripstone_cluster(ctx: &mut Ctx, c: &DripstoneCluster, origin: Pos) -> bool {
    if !is_empty_or_water(ctx.lv.get(origin)) {
        return false;
    }
    let i = c.height.sample(ctx.r);
    let f = c.wetness.sample(ctx.r);
    let g = c.density.sample(ctx.r);
    let j = c.radius.sample(ctx.r);
    let k = c.radius.sample(ctx.r);
    for l in -j..=j {
        for m in -k..=k {
            let d = chance_of_stalagmite_or_stalactite(j, k, l, m, c);
            let p2 = origin.offset(l, 0, m);
            place_column(ctx, c, p2, l, m, f, d, i, g);
        }
    }
    true
}

// --- large_dripstone -------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct WindOffsetter {
    origin_y: i32,
    wind_x: f64,
    wind_z: f64,
    blown: bool,
}

impl WindOffsetter {
    /// `WindOffsetter(int, RandomSource, FloatProvider)`.
    fn new(origin_y: i32, r: &mut WorldgenRandom, provider: &FloatProvider) -> Self {
        let f = provider.sample(r);
        let g = mth::random_between_f32(r, 0.0, std::f32::consts::PI);
        Self {
            origin_y,
            wind_x: (mth::cos(g as f64) * f) as f64,
            wind_z: (mth::sin(g as f64) * f) as f64,
            blown: true,
        }
    }

    /// `WindOffsetter.noWind`.
    fn none() -> Self {
        Self {
            origin_y: 0,
            wind_x: 0.0,
            wind_z: 0.0,
            blown: false,
        }
    }

    fn offset(&self, p: Pos) -> Pos {
        if !self.blown {
            return p;
        }
        let i = (self.origin_y - p.y) as f64;
        p.offset(mth::floor(self.wind_x * i), 0, mth::floor(self.wind_z * i))
    }
}

struct LargeDripstoneShape {
    root: Pos,
    pointing_up: bool,
    radius: i32,
    bluntness: f64,
    scale: f64,
}

impl LargeDripstoneShape {
    /// `makeDripstone`.
    fn make(
        root: Pos,
        pointing_up: bool,
        r: &mut WorldgenRandom,
        radius: i32,
        bluntness: &FloatProvider,
        scale: &FloatProvider,
    ) -> Self {
        Self {
            root,
            pointing_up,
            radius,
            bluntness: bluntness.sample(r) as f64,
            scale: scale.sample(r) as f64,
        }
    }

    fn get_height_at_radius(&self, f: f64) -> i32 {
        dripstone_height_curve(f, self.radius as f64, self.scale, self.bluntness) as i32
    }

    fn get_height(&self) -> i32 {
        self.get_height_at_radius(0.0)
    }

    fn is_suitable_for_wind(&self, c: &LargeDripstone) -> bool {
        self.radius >= c.min_radius_for_wind && self.bluntness >= c.min_bluntness_for_wind as f64
    }

    fn move_back_until_base_is_inside_stone_and_shrink_radius_if_necessary(
        &mut self,
        ctx: &mut Ctx,
        wind: &WindOffsetter,
    ) -> bool {
        while self.radius > 1 {
            let mut m = self.root;
            let i = self.get_height().min(10);
            for _ in 0..i {
                if blockinfo::is_lava(ctx.lv.get(m)) {
                    return false;
                }
                if is_circle_mostly_embedded_in_stone(ctx, wind.offset(m), self.radius) {
                    self.root = m;
                    return true;
                }
                m = m.rel(if self.pointing_up { Dir::Down } else { Dir::Up });
            }
            self.radius /= 2;
        }
        false
    }

    fn place_blocks(&self, ctx: &mut Ctx, wind: &WindOffsetter) {
        for i in -self.radius..=self.radius {
            for j in -self.radius..=self.radius {
                let f = ((i * i + j * j) as f32).sqrt();
                if f > self.radius as f32 {
                    continue;
                }
                let mut k = self.get_height_at_radius(f as f64);
                if k <= 0 {
                    continue;
                }
                if ctx.r.next_float() < 0.2 {
                    k = (k as f32 * mth::random_between_f32(ctx.r, 0.8, 1.0)) as i32;
                }
                let mut m = self.root.offset(i, 0, j);
                let mut bl = false;
                let l = if self.pointing_up {
                    ctx.lv.height(Heightmap::WorldSurfaceWg, m.x, m.z)
                } else {
                    i32::MAX
                };
                let mut n = 0;
                while n < k && m.y < l {
                    let p = wind.offset(m);
                    let s = ctx.lv.get(p);
                    if is_empty_or_water_or_lava(s) {
                        bl = true;
                        let b = dripstone_block();
                        ctx.lv.set(p, b);
                    } else if bl && ctx.tags().base_stone_overworld.contains(s) {
                        break;
                    }
                    m = m.rel(if self.pointing_up { Dir::Up } else { Dir::Down });
                    n += 1;
                }
            }
        }
    }
}

/// `LargeDripstoneFeature.place`.
pub(crate) fn large_dripstone(ctx: &mut Ctx, c: &LargeDripstone, origin: Pos) -> bool {
    if !is_empty_or_water(ctx.lv.get(origin)) {
        return false;
    }
    let drip = ctx.tags().dripstone_replaceable.clone();
    let solid = move |s: BlockStateId| {
        is(s, "minecraft:dripstone_block") || drip.contains(s) || blockinfo::is_lava(s)
    };
    let Some(column) = column_scan(
        ctx,
        origin,
        c.floor_to_ceiling_search_range,
        &is_empty_or_water,
        &solid,
    ) else {
        return false;
    };
    let (Some(ceiling), Some(floor)) = (column.ceiling, column.floor) else {
        return false;
    };
    let height = ceiling - floor - 1;
    if height < 4 {
        return false;
    }
    let i = (height as f32 * c.max_column_radius_to_cave_height_ratio) as i32;
    let j = i.clamp(c.column_radius.min_value(), c.column_radius.max_value());
    let k = mth_next_int(ctx.r, c.column_radius.min_value(), j);
    let mut stalactite = LargeDripstoneShape::make(
        origin.at_y(ceiling - 1),
        false,
        ctx.r,
        k,
        &c.stalactite_bluntness,
        &c.height_scale,
    );
    let mut stalagmite = LargeDripstoneShape::make(
        origin.at_y(floor + 1),
        true,
        ctx.r,
        k,
        &c.stalagmite_bluntness,
        &c.height_scale,
    );
    let wind = if stalactite.is_suitable_for_wind(c) && stalagmite.is_suitable_for_wind(c) {
        WindOffsetter::new(origin.y, ctx.r, &c.wind_speed)
    } else {
        WindOffsetter::none()
    };
    let bl =
        stalactite.move_back_until_base_is_inside_stone_and_shrink_radius_if_necessary(ctx, &wind);
    let bl2 =
        stalagmite.move_back_until_base_is_inside_stone_and_shrink_radius_if_necessary(ctx, &wind);
    if bl {
        stalactite.place_blocks(ctx, &wind);
    }
    if bl2 {
        stalagmite.place_blocks(ctx, &wind);
    }
    true
}
