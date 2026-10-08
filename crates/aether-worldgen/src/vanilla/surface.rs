//! Surface rules: the pass that turns bare stone into grass, dirt, sand,
//! gravel, terracotta, snow, deepslate and bedrock.
//!
//! Vanilla's noise stage ([`super::terrain`]) places only stone, water, lava
//! and ore. Everything else near the surface — and the bedrock floor — is
//! painted on afterwards by a data-driven rule tree living in
//! `noise_settings/overworld.json` under `surface_rule`. This module reads
//! that tree the same way [`super::density`] reads the noise graph: nothing
//! about which block goes where is hard-coded here, only the operator
//! semantics are — and those are a port of the game's `SurfaceRules` and
//! `SurfaceSystem`, including the parts that are code rather than data:
//!
//! * every condition type the overworld uses — `biome`, `noise_threshold`,
//!   `vertical_gradient`, `y_above`, `water`, `not`,
//!   `above_preliminary_surface`, `stone_depth`, `temperature`, `steep` and
//!   `hole`;
//! * every rule type — `sequence`, `condition`, `block` and `bandlands` (the
//!   badlands' seeded terracotta colour bands);
//! * the per-column walk that threads stone depth above and below, the water
//!   line and the jittered block-resolution biome through the tree;
//! * the two hard-coded column extensions: eroded-badlands hoodoo pillars and
//!   frozen-ocean icebergs.
//!
//! Measured against the game's own `buildSurface` over a `ProtoChunk`; see
//! `examples/vanilla_stage_parity.rs`.

use std::collections::HashSet;
use std::sync::Arc;

use aether_world::BlockStateId;

use super::biome::{BiomeId, BiomeRegistry};
use super::blockinfo;
use super::chunk::ProtoChunk;
use super::density::{BuildError, NoiseRegistry};
use super::json::Json;
use super::noise::NormalNoise;
use super::random::PositionalFactory;
use super::rng::Rng;

/// A Y anchor as `noise_settings` writes it: an absolute Y, or relative to the
/// dimension's floor or ceiling.
#[derive(Debug, Clone, Copy)]
pub enum Anchor {
    /// `absolute`.
    Absolute(i32),
    /// `above_bottom`.
    AboveBottom(i32),
    /// `below_top`.
    BelowTop(i32),
}

impl Anchor {
    /// Parse `{"absolute": n}` / `{"above_bottom": n}` / `{"below_top": n}`.
    pub fn parse(j: &Json) -> Result<Self, BuildError> {
        if let Some(v) = j.get("absolute").and_then(Json::as_f64) {
            return Ok(Anchor::Absolute(v as i32));
        }
        if let Some(v) = j.get("above_bottom").and_then(Json::as_f64) {
            return Ok(Anchor::AboveBottom(v as i32));
        }
        if let Some(v) = j.get("below_top").and_then(Json::as_f64) {
            return Ok(Anchor::BelowTop(v as i32));
        }
        Err(BuildError::new(format!("unrecognized y anchor {j:?}")))
    }

    /// `VerticalAnchor.resolveY`.
    pub fn resolve(self, min_y: i32, height: i32) -> i32 {
        match self {
            Anchor::Absolute(v) => v,
            Anchor::AboveBottom(v) => min_y + v,
            Anchor::BelowTop(v) => min_y + height - 1 - v,
        }
    }
}

/// One leaf of `if_true`.
enum Cond {
    Biome(Vec<BiomeId>),
    NoiseThreshold {
        noise: Arc<NormalNoise>,
        min: f64,
        max: f64,
    },
    VerticalGradient {
        true_y: i32,
        false_y: i32,
        random: PositionalFactory,
    },
    YAbove {
        anchor_y: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    Water {
        offset: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    Not(Box<Cond>),
    AbovePreliminarySurface,
    StoneDepth {
        ceiling: bool,
        add_surface_depth: bool,
        offset: i32,
        secondary_depth_range: i32,
    },
    Temperature,
    Steep,
    Hole,
    /// A condition type not in this port. Always false; listed by
    /// [`SurfaceSystem::unsupported`].
    Unsupported,
}

/// One node of the rule tree.
enum Rule {
    Sequence(Vec<Rule>),
    Condition(Cond, Box<Rule>),
    Block(BlockStateId),
    Bandlands,
    /// An unknown rule type, or a block this registry lacks.
    NoOp,
}

fn rule_type(j: &Json) -> Result<&str, BuildError> {
    j.get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| BuildError::new("surface rule: node has no `type`"))
}

/// Resolve a `{"Name": ..., "Properties": {...}}` block state.
pub fn parse_block_state(j: &Json) -> Option<BlockStateId> {
    let name = j.str_of("Name")?;
    let mut s = name.to_string();
    if let Some(Json::Obj(p)) = j.get("Properties") {
        let parts: Vec<String> = p
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|v| format!("{k}={v}")))
            .collect();
        if !parts.is_empty() {
            s = format!("{name}[{}]", parts.join(","));
        }
    }
    blockinfo::parse_state(&s)
}

struct Loader<'a> {
    noises: &'a NoiseRegistry,
    biomes: &'a BiomeRegistry,
    min_y: i32,
    height: i32,
    unsupported: HashSet<String>,
}

impl Loader<'_> {
    fn parse_rule(&mut self, j: &Json) -> Result<Rule, BuildError> {
        match rule_type(j)?.trim_start_matches("minecraft:") {
            "sequence" => {
                let seq = j
                    .get("sequence")
                    .and_then(Json::as_arr)
                    .ok_or_else(|| BuildError::new("sequence rule: no `sequence` array"))?;
                let mut out = Vec::with_capacity(seq.len());
                for r in seq {
                    out.push(self.parse_rule(r)?);
                }
                Ok(Rule::Sequence(out))
            }
            "condition" => {
                let if_true = j
                    .get("if_true")
                    .ok_or_else(|| BuildError::new("condition rule: no `if_true`"))?;
                let then_run = j
                    .get("then_run")
                    .ok_or_else(|| BuildError::new("condition rule: no `then_run`"))?;
                let cond = self.parse_cond(if_true)?;
                let then = self.parse_rule(then_run)?;
                Ok(Rule::Condition(cond, Box::new(then)))
            }
            "block" => {
                let st = j
                    .get("result_state")
                    .ok_or_else(|| BuildError::new("block rule: no `result_state`"))?;
                // A block this registry lacks is a named gap, not a reason to
                // place a substitute: the leaf falls through.
                match parse_block_state(st) {
                    Some(id) => Ok(Rule::Block(id)),
                    None => {
                        self.unsupported
                            .insert(format!("block:{}", st.str_of("Name").unwrap_or("?")));
                        Ok(Rule::NoOp)
                    }
                }
            }
            "bandlands" => Ok(Rule::Bandlands),
            other => {
                self.unsupported.insert(format!("rule:{other}"));
                Ok(Rule::NoOp)
            }
        }
    }

    fn parse_cond(&mut self, j: &Json) -> Result<Cond, BuildError> {
        Ok(match rule_type(j)?.trim_start_matches("minecraft:") {
            "biome" => {
                let v = j
                    .get("biome_is")
                    .ok_or_else(|| BuildError::new("biome condition: no `biome_is`"))?;
                let names: Vec<&str> = match v {
                    Json::Str(s) => vec![s.as_str()],
                    Json::Arr(a) => a.iter().filter_map(Json::as_str).collect(),
                    _ => Vec::new(),
                };
                Cond::Biome(names.iter().filter_map(|n| self.biomes.id(n)).collect())
            }
            "noise_threshold" => {
                let name = j
                    .str_of("noise")
                    .ok_or_else(|| BuildError::new("noise_threshold: no `noise`"))?;
                Cond::NoiseThreshold {
                    noise: self.noises.get(name)?,
                    min: j.f64_or("min_threshold", f64::NEG_INFINITY),
                    max: j.f64_or("max_threshold", f64::INFINITY),
                }
            }
            "vertical_gradient" => {
                let t = Anchor::parse(
                    j.get("true_at_and_below")
                        .ok_or_else(|| BuildError::new("vertical_gradient: no `true_at_and_below`"))?,
                )?;
                let f = Anchor::parse(
                    j.get("false_at_and_above")
                        .ok_or_else(|| BuildError::new("vertical_gradient: no `false_at_and_above`"))?,
                )?;
                let name = j
                    .str_of("random_name")
                    .ok_or_else(|| BuildError::new("vertical_gradient: no `random_name`"))?;
                let name = if name.contains(':') {
                    name.to_string()
                } else {
                    format!("minecraft:{name}")
                };
                Cond::VerticalGradient {
                    true_y: t.resolve(self.min_y, self.height),
                    false_y: f.resolve(self.min_y, self.height),
                    random: self.noises.forked_factory(&name),
                }
            }
            "y_above" => {
                let anchor = Anchor::parse(
                    j.get("anchor")
                        .ok_or_else(|| BuildError::new("y_above: no `anchor`"))?,
                )?;
                Cond::YAbove {
                    anchor_y: anchor.resolve(self.min_y, self.height),
                    surface_depth_multiplier: j.i32_or("surface_depth_multiplier", 0),
                    add_stone_depth: j.bool_or("add_stone_depth", false),
                }
            }
            "water" => Cond::Water {
                offset: j.i32_or("offset", 0),
                surface_depth_multiplier: j.i32_or("surface_depth_multiplier", 0),
                add_stone_depth: j.bool_or("add_stone_depth", false),
            },
            "not" => {
                let inner = j
                    .get("invert")
                    .ok_or_else(|| BuildError::new("not condition: no `invert`"))?;
                Cond::Not(Box::new(self.parse_cond(inner)?))
            }
            "above_preliminary_surface" => Cond::AbovePreliminarySurface,
            "stone_depth" => Cond::StoneDepth {
                ceiling: j.str_of("surface_type") == Some("ceiling"),
                add_surface_depth: j.bool_or("add_surface_depth", false),
                offset: j.i32_or("offset", 0),
                secondary_depth_range: j.i32_or("secondary_depth_range", 0),
            },
            "temperature" => Cond::Temperature,
            "steep" => Cond::Steep,
            "hole" => Cond::Hole,
            other => {
                self.unsupported.insert(other.to_string());
                Cond::Unsupported
            }
        })
    }
}

/// What the surface pass needs from the rest of the generator.
pub trait SurfaceEnv {
    /// The biome at a block, through the jittered `BiomeManager` lookup.
    fn biome_at_block(&self, x: i32, y: i32, z: i32) -> BiomeId;
    /// `NoiseChunk.preliminarySurfaceLevel(x, z)`.
    fn preliminary_surface_level(&self, x: i32, z: i32) -> i32;
}

/// The surface system: the rule tree plus the noises and bands it reads.
pub struct SurfaceSystem {
    root: Rule,
    surface_noise: Arc<NormalNoise>,
    surface_secondary_noise: Arc<NormalNoise>,
    clay_bands_offset: Arc<NormalNoise>,
    badlands_pillar: Arc<NormalNoise>,
    badlands_pillar_roof: Arc<NormalNoise>,
    badlands_surface: Arc<NormalNoise>,
    iceberg_pillar: Arc<NormalNoise>,
    iceberg_pillar_roof: Arc<NormalNoise>,
    iceberg_surface: Arc<NormalNoise>,
    noise_random: PositionalFactory,
    clay_bands: Vec<BlockStateId>,
    default_block: BlockStateId,
    snow_block: BlockStateId,
    packed_ice: BlockStateId,
    water_block: u16,
    sea_level: i32,
    min_y: i32,
    height: i32,
    eroded_badlands: Option<BiomeId>,
    frozen_oceans: [Option<BiomeId>; 2],
    unsupported: Vec<String>,
}

fn state(name: &str) -> Result<BlockStateId, BuildError> {
    blockinfo::parse_state(name).ok_or_else(|| BuildError::new(format!("block registry has no `{name}`")))
}

impl SurfaceSystem {
    /// Build from the `surface_rule` object out of a dimension's
    /// `noise_settings` file.
    pub fn load(
        surface_rule: &Json,
        noises: &NoiseRegistry,
        biomes: &BiomeRegistry,
        min_y: i32,
        height: i32,
        sea_level: i32,
    ) -> Result<Self, BuildError> {
        let mut loader = Loader {
            noises,
            biomes,
            min_y,
            height,
            unsupported: HashSet::new(),
        };
        let root = loader.parse_rule(surface_rule)?;
        let mut unsupported: Vec<String> = loader.unsupported.into_iter().collect();
        unsupported.sort();
        let factory = noises.factory();
        let clay_bands = generate_bands(&mut factory.from_hash_of("minecraft:clay_bands"))?;
        Ok(Self {
            root,
            surface_noise: noises.get("minecraft:surface")?,
            surface_secondary_noise: noises.get("minecraft:surface_secondary")?,
            clay_bands_offset: noises.get("minecraft:clay_bands_offset")?,
            badlands_pillar: noises.get("minecraft:badlands_pillar")?,
            badlands_pillar_roof: noises.get("minecraft:badlands_pillar_roof")?,
            badlands_surface: noises.get("minecraft:badlands_surface")?,
            iceberg_pillar: noises.get("minecraft:iceberg_pillar")?,
            iceberg_pillar_roof: noises.get("minecraft:iceberg_pillar_roof")?,
            iceberg_surface: noises.get("minecraft:iceberg_surface")?,
            noise_random: factory,
            clay_bands,
            default_block: state("minecraft:stone")?,
            snow_block: state("minecraft:snow_block")?,
            packed_ice: state("minecraft:packed_ice")?,
            water_block: blockinfo::block_of(state("minecraft:water")?),
            sea_level,
            min_y,
            height,
            eroded_badlands: biomes.id("minecraft:eroded_badlands"),
            frozen_oceans: [biomes.id("minecraft:frozen_ocean"), biomes.id("minecraft:deep_frozen_ocean")],
            unsupported,
        })
    }

    /// The condition and rule types this tree used but this port does not
    /// evaluate. Empty for the 1.21.11 overworld.
    pub fn unsupported(&self) -> &[String] {
        &self.unsupported
    }

    /// `getSurfaceDepth`.
    pub fn surface_depth(&self, x: i32, z: i32) -> i32 {
        let n = self.surface_noise.get_value(x as f64, 0.0, z as f64);
        (n * 2.75 + 3.0 + self.noise_random.at(x, 0, z).next_double() * 0.25) as i32
    }

    /// `getBand`.
    fn band(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        let off = (self.clay_bands_offset.get_value(x as f64, 0.0, z as f64) * 4.0).round() as i32;
        let n = self.clay_bands.len() as i32;
        self.clay_bands[((y + off + n).rem_euclid(n)) as usize]
    }

    /// The top material the rule tree would give a block at `(x, y, z)` with
    /// stone depth 1 — vanilla's `topMaterial`, used by carvers to re-grass
    /// the floor they expose.
    pub fn top_material(
        &self,
        chunk: &ProtoChunk,
        env: &impl SurfaceEnv,
        biomes: &BiomeRegistry,
        x: i32,
        y: i32,
        z: i32,
        under_fluid: bool,
    ) -> Option<BlockStateId> {
        let col = ColumnCtx::new(self, chunk, env, biomes, x, z);
        let ctx = BlockCtx {
            col: &col,
            y,
            stone_depth_above: 1,
            stone_depth_below: 1,
            water_height: if under_fluid { y + 1 } else { i32::MIN },
            biome: std::cell::Cell::new(None),
        };
        self.eval_rule(&self.root, &ctx)
    }

    /// Run the surface pass over a chunk — vanilla's `buildSurface`.
    pub fn build(&self, chunk: &mut ProtoChunk, env: &impl SurfaceEnv, biomes: &BiomeRegistry) {
        let min_y = chunk.min_y;
        for lx in 0..16usize {
            for lz in 0..16usize {
                let x = chunk.min_x() + lx as i32;
                let z = chunk.min_z() + lz as i32;
                let top = chunk.world_surface(lx, lz) + 2;
                let column_biome = env.biome_at_block(x, top, z);
                if Some(column_biome) == self.eroded_badlands {
                    self.eroded_badlands_extension(chunk, lx, lz, x, z, top);
                }
                let start = chunk.world_surface(lx, lz) + 2;
                let col = ColumnCtx::new(self, chunk, env, biomes, x, z);
                let mut writes: Vec<(i32, BlockStateId)> = Vec::new();
                {
                    let mut stone_above = 0i32;
                    let mut water_height = i32::MIN;
                    let mut run_bottom = i32::MAX;
                    let mut y = start;
                    while y >= min_y {
                        let b = chunk.get(lx, y, lz);
                        if blockinfo::is_air(b) {
                            stone_above = 0;
                            water_height = i32::MIN;
                        } else if blockinfo::has_fluid(b) {
                            if water_height == i32::MIN {
                                water_height = y + 1;
                            }
                        } else {
                            if run_bottom >= y {
                                run_bottom = -2_032; // WAY_BELOW_MIN_Y; always overwritten
                                let mut j = y - 1;
                                while j >= min_y - 1 {
                                    let s = chunk.get(lx, j, lz);
                                    if blockinfo::is_air(s) || blockinfo::has_fluid(s) {
                                        run_bottom = j + 1;
                                        break;
                                    }
                                    j -= 1;
                                }
                            }
                            stone_above += 1;
                            if b == self.default_block {
                                let ctx = BlockCtx {
                                    col: &col,
                                    y,
                                    stone_depth_above: stone_above,
                                    stone_depth_below: y - run_bottom + 1,
                                    water_height,
                                    biome: std::cell::Cell::new(None),
                                };
                                if let Some(s) = self.eval_rule(&self.root, &ctx) {
                                    writes.push((y, s));
                                }
                            }
                        }
                        y -= 1;
                    }
                }
                let min_surface = col.min_surface_level();
                drop(col);
                // The walk only ever reads blocks below the one it may have
                // replaced, and replacing stone with another solid never
                // changes what the walk sees, so writes can be applied after.
                for (y, s) in writes {
                    chunk.set(lx, y, lz, s);
                }
                if self.frozen_oceans.contains(&Some(column_biome)) {
                    self.frozen_ocean_extension(chunk, biomes, column_biome, min_surface, lx, lz, x, z, top);
                }
            }
        }
    }

    fn eroded_badlands_extension(&self, chunk: &mut ProtoChunk, lx: usize, lz: usize, x: i32, z: i32, top: i32) {
        let a = (self.badlands_surface.get_value(x as f64, 0.0, z as f64) * 8.25).abs();
        let b = self.badlands_pillar.get_value(x as f64 * 0.2, 0.0, z as f64 * 0.2) * 15.0;
        let d = a.min(b);
        if d <= 0.0 {
            return;
        }
        let roof = (self
            .badlands_pillar_roof
            .get_value(x as f64 * 0.75, 0.0, z as f64 * 0.75)
            * 1.5)
            .abs();
        let h = 64.0 + (d * d * 2.5).min((roof * 50.0).ceil() + 24.0);
        let pillar_top = h.floor() as i32;
        if top > pillar_top {
            return;
        }
        let stone_block = blockinfo::block_of(self.default_block);
        let mut y = pillar_top;
        while y >= chunk.min_y {
            let s = chunk.get(lx, y, lz);
            if blockinfo::block_of(s) == stone_block {
                break;
            }
            if blockinfo::block_of(s) == self.water_block {
                return;
            }
            y -= 1;
        }
        let mut y = pillar_top;
        while y >= chunk.min_y && blockinfo::is_air(chunk.get(lx, y, lz)) {
            chunk.set(lx, y, lz, self.default_block);
            y -= 1;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn frozen_ocean_extension(
        &self,
        chunk: &mut ProtoChunk,
        biomes: &BiomeRegistry,
        biome: BiomeId,
        min_surface: i32,
        lx: usize,
        lz: usize,
        x: i32,
        z: i32,
        top: i32,
    ) {
        let a = (self.iceberg_surface.get_value(x as f64, 0.0, z as f64) * 8.25).abs();
        let b = self.iceberg_pillar.get_value(x as f64 * 1.28, 0.0, z as f64 * 1.28) * 15.0;
        let d = a.min(b);
        if d <= 1.8 {
            return;
        }
        let roof = (self
            .iceberg_pillar_roof
            .get_value(x as f64 * 1.17, 0.0, z as f64 * 1.17)
            * 1.5)
            .abs();
        let mut peak = (d * d * 1.2).min((roof * 40.0).ceil() + 14.0);
        if biomes.should_melt_iceberg_slightly(biome, x, self.sea_level, z, self.sea_level) {
            peak -= 2.0;
        }
        let bottom;
        if peak > 2.0 {
            bottom = self.sea_level as f64 - peak - 7.0;
            peak += self.sea_level as f64;
        } else {
            peak = 0.0;
            bottom = 0.0;
        }
        let mut r = self.noise_random.at(x, 0, z);
        let snow_depth = 2 + r.next_int_bounded(4);
        let snow_line = self.sea_level + 18 + r.next_int_bounded(10);
        let mut placed = 0;
        let mut y = top.max(peak as i32 + 1);
        while y >= min_surface {
            let s = chunk.get(lx, y, lz);
            let air_ok = blockinfo::is_air(s) && y < peak as i32 && r.next_double() > 0.01;
            let water_ok = !air_ok
                && blockinfo::block_of(s) == self.water_block
                && y > bottom as i32
                && y < self.sea_level
                && bottom != 0.0
                && r.next_double() > 0.15;
            if air_ok || water_ok {
                if placed <= snow_depth && y > snow_line {
                    chunk.set(lx, y, lz, self.snow_block);
                    placed += 1;
                } else {
                    chunk.set(lx, y, lz, self.packed_ice);
                }
            }
            y -= 1;
        }
    }

    fn eval_rule<E: SurfaceEnv>(&self, rule: &Rule, ctx: &BlockCtx<E>) -> Option<BlockStateId> {
        match rule {
            Rule::Sequence(seq) => seq.iter().find_map(|r| self.eval_rule(r, ctx)),
            Rule::Condition(c, then) => {
                if self.eval_cond(c, ctx) {
                    self.eval_rule(then, ctx)
                } else {
                    None
                }
            }
            Rule::Block(id) => Some(*id),
            Rule::Bandlands => Some(self.band(ctx.col.x, ctx.y, ctx.col.z)),
            Rule::NoOp => None,
        }
    }

    fn eval_cond<E: SurfaceEnv>(&self, cond: &Cond, ctx: &BlockCtx<E>) -> bool {
        let col = ctx.col;
        match cond {
            Cond::Biome(list) => list.contains(&ctx.biome()),
            Cond::NoiseThreshold { noise, min, max } => {
                let v = noise.get_value(col.x as f64, 0.0, col.z as f64);
                v >= *min && v <= *max
            }
            Cond::VerticalGradient {
                true_y,
                false_y,
                random,
            } => {
                let y = ctx.y;
                if y <= *true_y {
                    true
                } else if y >= *false_y {
                    false
                } else {
                    let d = 1.0 + (y - true_y) as f64 / (false_y - true_y) as f64 * (0.0 - 1.0);
                    (random.at(col.x, y, col.z).next_float() as f64) < d
                }
            }
            Cond::YAbove {
                anchor_y,
                surface_depth_multiplier,
                add_stone_depth,
            } => {
                let adj = if *add_stone_depth { ctx.stone_depth_above } else { 0 };
                ctx.y + adj >= anchor_y + col.surface_depth * surface_depth_multiplier
            }
            Cond::Water {
                offset,
                surface_depth_multiplier,
                add_stone_depth,
            } => {
                if ctx.water_height == i32::MIN {
                    return true;
                }
                let adj = if *add_stone_depth { ctx.stone_depth_above } else { 0 };
                ctx.y + adj >= ctx.water_height + offset + col.surface_depth * surface_depth_multiplier
            }
            Cond::Not(inner) => !self.eval_cond(inner, ctx),
            Cond::AbovePreliminarySurface => ctx.y >= col.min_surface_level(),
            Cond::StoneDepth {
                ceiling,
                add_surface_depth,
                offset,
                secondary_depth_range,
            } => {
                let i = if *ceiling { ctx.stone_depth_below } else { ctx.stone_depth_above };
                let j = if *add_surface_depth { col.surface_depth } else { 0 };
                let k = if *secondary_depth_range == 0 {
                    0
                } else {
                    let t = (col.surface_secondary() - -1.0) / (1.0 - -1.0);
                    (0.0 + t * (*secondary_depth_range as f64 - 0.0)) as i32
                };
                i <= 1 + offset + j + k
            }
            Cond::Temperature => {
                col.biomes
                    .cold_enough_to_snow(ctx.biome(), col.x, ctx.y, col.z, self.sea_level)
            }
            Cond::Steep => col.steep(),
            Cond::Hole => col.surface_depth <= 0,
            Cond::Unsupported => false,
        }
    }
}

/// The per-column state of `SurfaceRules.Context`, with its lazily computed
/// members.
struct ColumnCtx<'a, E: SurfaceEnv> {
    sys: &'a SurfaceSystem,
    chunk: &'a ProtoChunk,
    env: &'a E,
    biomes: &'a BiomeRegistry,
    x: i32,
    z: i32,
    surface_depth: i32,
    surface_secondary: std::cell::Cell<Option<f64>>,
    min_surface: std::cell::Cell<Option<i32>>,
    steep: std::cell::Cell<Option<bool>>,
}

impl<'a, E: SurfaceEnv> ColumnCtx<'a, E> {
    fn new(sys: &'a SurfaceSystem, chunk: &'a ProtoChunk, env: &'a E, biomes: &'a BiomeRegistry, x: i32, z: i32) -> Self {
        Self {
            sys,
            chunk,
            env,
            biomes,
            x,
            z,
            surface_depth: sys.surface_depth(x, z),
            surface_secondary: std::cell::Cell::new(None),
            min_surface: std::cell::Cell::new(None),
            steep: std::cell::Cell::new(None),
        }
    }

    fn surface_secondary(&self) -> f64 {
        if let Some(v) = self.surface_secondary.get() {
            return v;
        }
        let v = self
            .sys
            .surface_secondary_noise
            .get_value(self.x as f64, 0.0, self.z as f64);
        self.surface_secondary.set(Some(v));
        v
    }

    /// `getMinSurfaceLevel`.
    fn min_surface_level(&self) -> i32 {
        if let Some(v) = self.min_surface.get() {
            return v;
        }
        let sx = self.x >> 4;
        let sz = self.z >> 4;
        let p = |cx: i32, cz: i32| self.env.preliminary_surface_level(cx << 4, cz << 4) as f64;
        let (a, b, c, d) = (p(sx, sz), p(sx + 1, sz), p(sx, sz + 1), p(sx + 1, sz + 1));
        let tx = ((self.x & 15) as f32 / 16.0) as f64;
        let tz = ((self.z & 15) as f32 / 16.0) as f64;
        let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
        let v = lerp(tz, lerp(tx, a, b), lerp(tx, c, d)).floor() as i32 + self.surface_depth - 8;
        self.min_surface.set(Some(v));
        v
    }

    /// `SteepMaterialCondition`.
    fn steep(&self) -> bool {
        if let Some(v) = self.steep.get() {
            return v;
        }
        let lx = (self.x & 15) as usize;
        let lz = (self.z & 15) as usize;
        let h = |x: usize, z: usize| self.chunk.world_surface(x, z) + 1;
        let n = h(lx, lz.saturating_sub(1));
        let s = h(lx, (lz + 1).min(15));
        let v = if s >= n + 4 {
            true
        } else {
            let w = h(lx.saturating_sub(1), lz);
            let e = h((lx + 1).min(15), lz);
            w >= e + 4
        };
        self.steep.set(Some(v));
        v
    }
}

struct BlockCtx<'c, 'a, E: SurfaceEnv> {
    col: &'c ColumnCtx<'a, E>,
    y: i32,
    stone_depth_above: i32,
    stone_depth_below: i32,
    water_height: i32,
    biome: std::cell::Cell<Option<BiomeId>>,
}

impl<E: SurfaceEnv> BlockCtx<'_, '_, E> {
    fn biome(&self) -> BiomeId {
        if let Some(b) = self.biome.get() {
            return b;
        }
        let b = self.col.env.biome_at_block(self.col.x, self.y, self.col.z);
        self.biome.set(Some(b));
        b
    }
}

/// `SurfaceSystem.generateBands`.
fn generate_bands(r: &mut impl Rng) -> Result<Vec<BlockStateId>, BuildError> {
    let terracotta = state("minecraft:terracotta")?;
    let orange = state("minecraft:orange_terracotta")?;
    let yellow = state("minecraft:yellow_terracotta")?;
    let brown = state("minecraft:brown_terracotta")?;
    let red = state("minecraft:red_terracotta")?;
    let white = state("minecraft:white_terracotta")?;
    let light_gray = state("minecraft:light_gray_terracotta")?;
    let mut bands = vec![terracotta; 192];
    let len = bands.len();
    let mut i = 0usize;
    while i < len {
        i += r.next_int_bounded(5) as usize + 1;
        if i < len {
            bands[i] = orange;
        }
        i += 1;
    }
    make_bands(r, &mut bands, 1, yellow);
    make_bands(r, &mut bands, 2, brown);
    make_bands(r, &mut bands, 1, red);
    let count = r.next_int_between_inclusive(9, 15);
    let mut placed = 0;
    let mut i = 0usize;
    while placed < count && i < len {
        bands[i] = white;
        if i >= 2 && r.next_bool() {
            bands[i - 1] = light_gray;
        }
        if i + 1 < len && r.next_bool() {
            bands[i + 1] = light_gray;
        }
        placed += 1;
        i += r.next_int_bounded(16) as usize + 4;
    }
    Ok(bands)
}

fn make_bands(r: &mut impl Rng, bands: &mut [BlockStateId], base: i32, s: BlockStateId) {
    let n = r.next_int_between_inclusive(6, 15);
    for _ in 0..n {
        let w = base + r.next_int_bounded(3);
        let start = r.next_int_bounded(bands.len() as i32) as usize;
        let mut k = 0usize;
        while start + k < bands.len() && (k as i32) < w {
            bands[start + k] = s;
            k += 1;
        }
    }
}
