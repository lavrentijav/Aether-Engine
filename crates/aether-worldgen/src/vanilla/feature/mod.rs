//! Decoration: the feature step, a port of `ChunkGenerator.applyBiomeDecoration`
//! and the placed/configured feature system under it.
//!
//! Everything is read from the pack: each biome's eleven per-step lists of
//! placed features, each placed feature's placement modifiers, each
//! configured feature's type and config. The feature *types* are code; the
//! ones the overworld uses are ported here from the game's classes, drawing
//! from the random source in the game's order:
//!
//! * the decoration seed (`setDecorationSeed`) and per-feature seed
//!   (`setFeatureSeed(seed, index, step)`), with the index taken from a port
//!   of `FeatureSorter`'s global ordering, so each feature starts from the
//!   same random state as in the game;
//! * placement modifiers evaluated depth-first, as Java's lazy
//!   `Stream.flatMap` chain evaluates them;
//! * trees, patches, ores, columns, disks, springs, lakes, snow and ice and
//!   the rest in [`features`] and [`tree`].
//!
//! What cannot be exact: the game decorates a chunk once its neighbours have
//! *terrain*, in whatever order chunks happen to load, so where two chunks'
//! features overlap the result depends on load order even in vanilla. Here
//! every chunk decorates against its neighbours' undecorated terrain, and the
//! writes are merged in a fixed order. Structures (villages, mineshafts, …)
//! are not generated.

pub mod blocks;
pub mod features;
pub mod level;
pub mod tree;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use aether_world::BlockStateId;

use super::biome::{BiomeId, BiomeRegistry};
use super::chunk::ProtoChunk;
use super::density::{BuildError, DataPack};
use super::generator::Core;
use super::json::Json;
use super::overworld::OverworldBiomeSource;
use super::providers::{HeightProvider, IntProvider};
use super::rng::{Rng, WorldgenRandom};
use super::simplex::BiomeNoises;
use super::tags::Tags;
use blocks::{BlockPredicate, CommonTags, LegacyNoises};
use features::Configured;
use level::{Dir, Heightmap, Level, Pos};

/// One placement modifier.
#[derive(Debug, Clone)]
pub enum Modifier {
    /// `count`.
    Count(IntProvider),
    /// `in_square`.
    InSquare,
    /// `heightmap`.
    Heightmap(Heightmap),
    /// `height_range`.
    HeightRange(HeightProvider),
    /// `rarity_filter`.
    Rarity(i32),
    /// `surface_water_depth_filter`.
    SurfaceWaterDepth(i32),
    /// `random_offset`.
    RandomOffset(IntProvider, IntProvider),
    /// `environment_scan`.
    EnvironmentScan {
        dir: Dir,
        target: BlockPredicate,
        allowed: BlockPredicate,
        max_steps: i32,
    },
    /// `noise_threshold_count`.
    NoiseThresholdCount { level: f64, below: i32, above: i32 },
    /// `noise_based_count`.
    NoiseBasedCount { ratio: i32, factor: f64, offset: f64 },
    /// `surface_relative_threshold_filter`.
    SurfaceRelative(Heightmap, i32, i32),
    /// `count_on_every_layer`.
    CountOnEveryLayer(IntProvider),
    /// `block_predicate_filter`.
    Predicate(BlockPredicate),
    /// `biome`.
    Biome,
    /// `fixed_placement`.
    Fixed(Vec<Pos>),
}

/// A placed feature: a configured feature plus where to put it.
#[derive(Debug)]
pub struct Placed {
    /// The registry id, when it has one (inline placed features do not).
    pub id: Option<String>,
    /// What is placed.
    pub feature: Arc<Configured>,
    /// Where.
    pub modifiers: Vec<Modifier>,
}

/// Parses the pack's features once.
pub(crate) struct Loader<'a> {
    pub pack: &'a DataPack,
    pub tags: &'a Tags,
    pub noises: LegacyNoises<'a>,
    pub min_y: i32,
    pub height: i32,
    placed: HashMap<String, Arc<Placed>>,
    configured: HashMap<String, Arc<Configured>>,
    pub unsupported: BTreeSet<String>,
}

fn ty(j: &Json) -> &str {
    j.str_of("type").unwrap_or("").trim_start_matches("minecraft:")
}

impl Loader<'_> {
    /// A placed feature by id or inline.
    pub fn placed(&mut self, j: &Json) -> Result<Arc<Placed>, BuildError> {
        if let Json::Str(id) = j {
            if let Some(p) = self.placed.get(id) {
                return Ok(Arc::clone(p));
            }
            let pj = self.pack.read_json("placed_feature", id)?;
            let p = self.placed_inline(&pj, Some(id.clone()))?;
            self.placed.insert(id.clone(), Arc::clone(&p));
            return Ok(p);
        }
        self.placed_inline(j, None)
    }

    fn placed_inline(&mut self, j: &Json, id: Option<String>) -> Result<Arc<Placed>, BuildError> {
        let feature = self.configured(j.get("feature").ok_or_else(|| BuildError::new("placed feature: no feature"))?)?;
        let mut modifiers = Vec::new();
        for m in j.get("placement").and_then(Json::as_arr).unwrap_or(&[]) {
            match self.modifier(m) {
                Ok(m) => modifiers.push(m),
                Err(e) => {
                    self.unsupported.insert(format!("placement: {e}"));
                }
            }
        }
        Ok(Arc::new(Placed { id, feature, modifiers }))
    }

    /// A configured feature by id or inline.
    pub fn configured(&mut self, j: &Json) -> Result<Arc<Configured>, BuildError> {
        if let Json::Str(id) = j {
            if let Some(c) = self.configured.get(id) {
                return Ok(Arc::clone(c));
            }
            let cj = self.pack.read_json("configured_feature", id)?;
            let c = Arc::new(features::parse(self, &cj).map_err(|e| BuildError::new(format!("{id}: {e}")))?);
            self.configured.insert(id.clone(), Arc::clone(&c));
            return Ok(c);
        }
        Ok(Arc::new(features::parse(self, j)?))
    }

    /// A block predicate.
    pub fn predicate(&self, j: &Json) -> Result<BlockPredicate, BuildError> {
        BlockPredicate::parse(j, self.tags)
    }

    fn modifier(&mut self, j: &Json) -> Result<Modifier, BuildError> {
        let get = |k: &str| j.get(k).ok_or_else(|| BuildError::new(format!("{}: no `{k}`", ty(j))));
        Ok(match ty(j) {
            "count" => Modifier::Count(IntProvider::parse(get("count")?)?),
            "in_square" => Modifier::InSquare,
            "heightmap" => Modifier::Heightmap(
                Heightmap::parse(j.str_of("heightmap").unwrap_or("")).ok_or_else(|| BuildError::new("bad heightmap"))?,
            ),
            "height_range" => Modifier::HeightRange(HeightProvider::parse(get("height")?, self.min_y, self.height)?),
            "rarity_filter" => Modifier::Rarity(j.i32_or("chance", 1)),
            "surface_water_depth_filter" => Modifier::SurfaceWaterDepth(j.i32_or("max_water_depth", 0)),
            "random_offset" => Modifier::RandomOffset(
                IntProvider::parse(get("xz_spread")?)?,
                IntProvider::parse(get("y_spread")?)?,
            ),
            "environment_scan" => Modifier::EnvironmentScan {
                dir: Dir::parse(j.str_of("direction_of_search").unwrap_or("down")).unwrap_or(Dir::Down),
                target: self.predicate(get("target_condition")?)?,
                allowed: match j.get("allowed_search_condition") {
                    Some(a) => self.predicate(a)?,
                    None => BlockPredicate::True,
                },
                max_steps: j.i32_or("max_steps", 1),
            },
            "noise_threshold_count" => Modifier::NoiseThresholdCount {
                level: j.f64_or("noise_level", 0.0),
                below: j.i32_or("below_noise", 0),
                above: j.i32_or("above_noise", 0),
            },
            "noise_based_count" => Modifier::NoiseBasedCount {
                ratio: j.i32_or("noise_to_count_ratio", 1),
                factor: j.f64_or("noise_factor", 1.0),
                offset: j.f64_or("noise_offset", 0.0),
            },
            "surface_relative_threshold_filter" => Modifier::SurfaceRelative(
                Heightmap::parse(j.str_of("heightmap").unwrap_or("")).ok_or_else(|| BuildError::new("bad heightmap"))?,
                j.i32_or("min_inclusive", i32::MIN),
                j.i32_or("max_inclusive", i32::MAX),
            ),
            "count_on_every_layer" => Modifier::CountOnEveryLayer(IntProvider::parse(get("count")?)?),
            "block_predicate_filter" => Modifier::Predicate(self.predicate(get("predicate")?)?),
            "biome" => Modifier::Biome,
            "fixed_placement" => Modifier::Fixed(
                j.get("positions")
                    .and_then(Json::as_arr)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|p| {
                        let a = p.as_arr()?;
                        Some(Pos::new(a.first()?.as_f64()? as i32, a.get(1)?.as_f64()? as i32, a.get(2)?.as_f64()? as i32))
                    })
                    .collect(),
            ),
            other => return Err(BuildError::new(format!("unknown placement modifier `{other}`"))),
        })
    }
}

/// What a feature placement can reach.
pub(crate) struct Ctx<'a, 'b> {
    pub lv: &'b mut Level<'a>,
    pub r: &'b mut WorldgenRandom,
    pub d: &'b Decorator,
}

impl Ctx<'_, '_> {
    /// The common tags.
    pub fn tags(&self) -> &CommonTags {
        &self.d.tags
    }
}

/// The decoration stage.
pub struct Decorator {
    /// Per step, the globally ordered features.
    steps: Vec<Vec<Arc<Placed>>>,
    /// Per biome, per step, the indices into `steps[step]` it runs.
    biome_steps: Vec<Vec<Vec<usize>>>,
    /// Per biome, every placed-feature id it has (`hasFeature`).
    biome_has: Vec<HashSet<String>>,
    pub(crate) tags: CommonTags,
    pub(crate) biome_noises: BiomeNoises,
    pub(crate) block_tags: Tags,
    unsupported: Mutex<BTreeSet<String>>,
}

impl Decorator {
    /// Read every biome's features and order them as `FeatureSorter` does.
    pub fn load(
        pack: &DataPack,
        biomes: &BiomeRegistry,
        source: &OverworldBiomeSource,
        entry_ids: &[BiomeId],
        min_y: i32,
        height: i32,
        noises: &super::density::NoiseRegistry,
    ) -> Result<Self, BuildError> {
        let tags = Tags::new(pack);
        let mut loader = Loader {
            pack,
            tags: &tags,
            noises: LegacyNoises { _noises: noises },
            min_y,
            height,
            placed: HashMap::new(),
            configured: HashMap::new(),
            unsupported: BTreeSet::new(),
        };
        // `possibleBiomes`: the biome table's biomes in first-appearance order.
        let mut possible: Vec<BiomeId> = Vec::new();
        let mut seen = HashSet::new();
        for (i, _) in source.biomes().entries().iter().enumerate() {
            let b = entry_ids[i];
            if seen.insert(b) {
                possible.push(b);
            }
        }
        // Every biome's lists, parsed.
        let mut per_biome: Vec<Vec<Vec<Arc<Placed>>>> = vec![Vec::new(); biomes.all().len()];
        for &b in &possible {
            let info = biomes.get(b);
            let mut steps = Vec::new();
            for step in &info.features {
                let mut list = Vec::new();
                for id in step {
                    match loader.placed(&Json::Str(id.clone())) {
                        Ok(p) => list.push(p),
                        Err(e) => {
                            loader.unsupported.insert(format!("{id}: {e}"));
                        }
                    }
                }
                steps.push(list);
            }
            per_biome[b as usize] = steps;
        }
        let sorted = sort_features(&possible, &per_biome);
        let mut index: Vec<HashMap<String, usize>> = Vec::new();
        for step in &sorted {
            index.push(
                step.iter()
                    .enumerate()
                    .map(|(i, p)| (p.id.clone().unwrap_or_default(), i))
                    .collect(),
            );
        }
        let mut biome_steps = vec![Vec::new(); biomes.all().len()];
        let mut biome_has = vec![HashSet::new(); biomes.all().len()];
        for &b in &possible {
            let mut out = Vec::new();
            for (s, list) in per_biome[b as usize].iter().enumerate() {
                let mut v: Vec<usize> = list
                    .iter()
                    .filter_map(|p| index.get(s).and_then(|m| m.get(p.id.as_deref().unwrap_or(""))).copied())
                    .collect();
                v.sort_unstable();
                v.dedup();
                out.push(v);
                for p in list {
                    if let Some(id) = &p.id {
                        biome_has[b as usize].insert(id.clone());
                    }
                }
            }
            biome_steps[b as usize] = out;
        }
        let unsupported = std::mem::take(&mut loader.unsupported);
        drop(loader);
        Ok(Self {
            steps: sorted,
            biome_steps,
            biome_has,
            tags: CommonTags::new(&tags),
            biome_noises: BiomeNoises::new(),
            block_tags: tags,
            unsupported: Mutex::new(unsupported),
        })
    }

    /// Feature and placement types the pack uses that the decorator skips.
    pub fn unsupported(&self) -> Vec<String> {
        self.unsupported.lock().unwrap().iter().cloned().collect()
    }

    /// Decorate chunk `(cx, cz)`; returns every block it wrote, anywhere in
    /// its 3×3 neighbourhood.
    pub(crate) fn decorate(
        &self,
        core: &Core,
        cx: i32,
        cz: i32,
        region: &[[Arc<ProtoChunk>; 3]; 3],
    ) -> Vec<(i32, i32, i32, BlockStateId)> {
        let mut lv = Level::new(core, cx, cz, region);
        let mut r = WorldgenRandom::xoroshiro(0);
        let (x0, z0) = (cx * 16, cz * 16);
        let deco_seed = r.set_decoration_seed(core.seed, x0, z0);
        let biomes = lv.stored_biomes();
        let origin = Pos::new(x0, core.min_y, z0);
        for (step, list) in self.steps.iter().enumerate() {
            let mut idx: BTreeSet<usize> = BTreeSet::new();
            for b in &biomes {
                if let Some(v) = self.biome_steps[*b as usize].get(step) {
                    idx.extend(v.iter().copied());
                }
            }
            for i in idx {
                r.set_feature_seed(deco_seed, i as i32, step as i32);
                let p = &list[i];
                let mut ctx = Ctx {
                    lv: &mut lv,
                    r: &mut r,
                    d: self,
                };
                place_placed(&mut ctx, p, origin, p.id.as_deref());
            }
        }
        lv.into_writes()
    }

    pub(crate) fn note_unsupported(&self, what: &str) {
        let mut u = self.unsupported.lock().unwrap();
        if !u.contains(what) {
            u.insert(what.to_string());
        }
    }
}

/// `PlacedFeature.place` / `placeWithBiomeCheck`.
pub(crate) fn place_placed(ctx: &mut Ctx, p: &Placed, origin: Pos, top: Option<&str>) -> bool {
    let mut any = false;
    run_modifiers(ctx, &p.modifiers, origin, top, &mut |ctx, pos| {
        if features::place_configured(ctx, &p.feature, pos) {
            any = true;
        }
    });
    any
}

/// The modifier chain, depth first — Java's lazy `flatMap` evaluates each
/// position all the way down before producing the next.
fn run_modifiers(ctx: &mut Ctx, mods: &[Modifier], pos: Pos, top: Option<&str>, f: &mut dyn FnMut(&mut Ctx, Pos)) {
    let Some((m, rest)) = mods.split_first() else {
        f(ctx, pos);
        return;
    };
    let mut next = |ctx: &mut Ctx, p: Pos| run_modifiers(ctx, rest, p, top, f);
    match m {
        Modifier::Count(c) => {
            let n = c.sample(ctx.r);
            for _ in 0..n {
                next(ctx, pos);
            }
        }
        Modifier::InSquare => {
            let x = ctx.r.next_int_bounded(16) + pos.x;
            let z = ctx.r.next_int_bounded(16) + pos.z;
            next(ctx, Pos::new(x, pos.y, z));
        }
        Modifier::Heightmap(t) => {
            let y = ctx.lv.height(*t, pos.x, pos.z);
            if y > ctx.lv.min_y() {
                next(ctx, Pos::new(pos.x, y, pos.z));
            }
        }
        Modifier::HeightRange(h) => {
            let y = h.sample(ctx.r);
            next(ctx, Pos::new(pos.x, y, pos.z));
        }
        Modifier::Rarity(chance) => {
            if ctx.r.next_float() < 1.0 / *chance as f32 {
                next(ctx, pos);
            }
        }
        Modifier::SurfaceWaterDepth(max) => {
            let floor = ctx.lv.height(Heightmap::OceanFloor, pos.x, pos.z);
            let surface = ctx.lv.height(Heightmap::WorldSurface, pos.x, pos.z);
            if surface - floor <= *max {
                next(ctx, pos);
            }
        }
        Modifier::RandomOffset(xz, y) => {
            let x = pos.x + xz.sample(ctx.r);
            let yy = pos.y + y.sample(ctx.r);
            let z = pos.z + xz.sample(ctx.r);
            next(ctx, Pos::new(x, yy, z));
        }
        Modifier::EnvironmentScan {
            dir,
            target,
            allowed,
            max_steps,
        } => {
            let t = &ctx.d.tags;
            if !allowed.test(ctx.lv, t, pos) {
                return;
            }
            let mut p = pos;
            for _ in 0..*max_steps {
                if target.test(ctx.lv, t, p) {
                    next(ctx, p);
                    return;
                }
                p = p.rel(*dir);
                if ctx.lv.outside_height(p.y) {
                    return;
                }
                if !allowed.test(ctx.lv, t, p) {
                    break;
                }
            }
            if target.test(ctx.lv, t, p) {
                next(ctx, p);
            }
        }
        Modifier::NoiseThresholdCount { level, below, above } => {
            let v = ctx
                .d
                .biome_noises
                .biome_info
                .get_value(pos.x as f64 / 200.0, pos.z as f64 / 200.0, false);
            let n = if v < *level { *below } else { *above };
            for _ in 0..n {
                next(ctx, pos);
            }
        }
        Modifier::NoiseBasedCount { ratio, factor, offset } => {
            let v = ctx
                .d
                .biome_noises
                .biome_info
                .get_value(pos.x as f64 / factor, pos.z as f64 / factor, false);
            let n = ((v + offset) * *ratio as f64).ceil() as i32;
            for _ in 0..n {
                next(ctx, pos);
            }
        }
        Modifier::SurfaceRelative(t, lo, hi) => {
            let h = ctx.lv.height(*t, pos.x, pos.z) as i64;
            if h + *lo as i64 <= pos.y as i64 && pos.y as i64 <= h + *hi as i64 {
                next(ctx, pos);
            }
        }
        Modifier::CountOnEveryLayer(c) => {
            let mut out = Vec::new();
            let mut layer = 0;
            loop {
                let mut found = false;
                let mut i = 0;
                while i < c.sample(ctx.r) {
                    let x = ctx.r.next_int_bounded(16) + pos.x;
                    let z = ctx.r.next_int_bounded(16) + pos.z;
                    let top_y = ctx.lv.height(Heightmap::MotionBlocking, x, z);
                    if let Some(y) = find_on_ground(ctx.lv, x, top_y, z, layer) {
                        out.push(Pos::new(x, y, z));
                        found = true;
                    }
                    i += 1;
                }
                layer += 1;
                if !found {
                    break;
                }
            }
            for p in out {
                next(ctx, p);
            }
        }
        Modifier::Predicate(pred) => {
            if pred.test(ctx.lv, &ctx.d.tags, pos) {
                next(ctx, pos);
            }
        }
        Modifier::Biome => {
            let Some(id) = top else {
                return;
            };
            let b = ctx.lv.biome(pos);
            if ctx.d.biome_has[b as usize].contains(id) {
                next(ctx, pos);
            }
        }
        Modifier::Fixed(list) => {
            let (cx, cz) = (pos.x >> 4, pos.z >> 4);
            for p in list.iter().filter(|p| p.x >> 4 == cx && p.z >> 4 == cz) {
                next(ctx, *p);
            }
        }
    }
}

fn find_on_ground(lv: &Level, x: i32, top: i32, z: i32, layer: i32) -> Option<i32> {
    let empty = |s: BlockStateId| {
        super::blockinfo::is_air(s) || matches!(super::blockinfo::name(s), "minecraft:water" | "minecraft:lava")
    };
    let mut seen = 0;
    let mut above = lv.get(Pos::new(x, top, z));
    let mut y = top;
    while y >= lv.min_y() + 1 {
        let s = lv.get(Pos::new(x, y - 1, z));
        if !empty(s) && empty(above) && super::blockinfo::name(s) != "minecraft:bedrock" {
            if seen == layer {
                return Some(y);
            }
            seen += 1;
        }
        above = s;
        y -= 1;
    }
    None
}

/// A port of `FeatureSorter.buildFeaturesPerStep`: one global order per step
/// that respects every biome's list order, found by a depth-first
/// topological sort over `(step, first-seen index)` keys.
fn sort_features(possible: &[BiomeId], per_biome: &[Vec<Vec<Arc<Placed>>>]) -> Vec<Vec<Arc<Placed>>> {
    // Feature identity is the placed feature's id.
    let mut feature_index: HashMap<String, usize> = HashMap::new();
    let mut by_index: Vec<Arc<Placed>> = Vec::new();
    // Node = (step, feature index); ordered as the game's comparator.
    let mut graph: BTreeMap<(usize, usize), BTreeSet<(usize, usize)>> = BTreeMap::new();
    let mut max_steps = 0;
    for &b in possible {
        let steps = &per_biome[b as usize];
        max_steps = max_steps.max(steps.len());
        let mut seq: Vec<(usize, usize)> = Vec::new();
        for (s, list) in steps.iter().enumerate() {
            for p in list {
                let key = p.id.clone().unwrap_or_default();
                let i = *feature_index.entry(key).or_insert_with(|| {
                    by_index.push(Arc::clone(p));
                    by_index.len() - 1
                });
                seq.push((s, i));
            }
        }
        for k in 0..seq.len() {
            let e = graph.entry(seq[k]).or_default();
            if k + 1 < seq.len() {
                e.insert(seq[k + 1]);
            }
        }
    }
    let mut done: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut path: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut order: Vec<(usize, usize)> = Vec::new();
    fn dfs(
        g: &BTreeMap<(usize, usize), BTreeSet<(usize, usize)>>,
        done: &mut BTreeSet<(usize, usize)>,
        path: &mut BTreeSet<(usize, usize)>,
        out: &mut Vec<(usize, usize)>,
        n: (usize, usize),
    ) -> bool {
        if done.contains(&n) {
            return false;
        }
        if path.contains(&n) {
            return true;
        }
        path.insert(n);
        if let Some(next) = g.get(&n) {
            for m in next {
                if dfs(g, done, path, out, *m) {
                    return true;
                }
            }
        }
        path.remove(&n);
        done.insert(n);
        out.push(n);
        false
    }
    let keys: Vec<(usize, usize)> = graph.keys().copied().collect();
    for k in keys {
        if !done.contains(&k) {
            // A cycle would be a broken pack; the game refuses to start.
            // Keep going with what is ordered so far.
            let _ = dfs(&graph, &mut done, &mut path, &mut order, k);
            path.clear();
        }
    }
    order.reverse();
    (0..max_steps)
        .map(|s| {
            order
                .iter()
                .filter(|(st, _)| *st == s)
                .map(|(_, i)| Arc::clone(&by_index[*i]))
                .collect()
        })
        .collect()
}
