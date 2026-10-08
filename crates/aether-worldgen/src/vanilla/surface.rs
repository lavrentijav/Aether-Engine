//! Surface rules: the pass that turns bare stone into grass, dirt, sand,
//! gravel, sandstone, deepslate and bedrock.
//!
//! Vanilla's noise stage ([`super::terrain`]) places only stone, water, lava
//! and ore. Everything else near the surface — and the bedrock floor — is
//! painted on afterwards by a data-driven rule tree living in
//! `noise_settings/overworld.json` under `surface_rule`. This module reads
//! that tree (`minecraft:sequence` / `minecraft:condition` / `minecraft:block`)
//! the same way [`super::density`] reads the noise graph: nothing about which
//! block goes where is hard-coded here, only the operator semantics are.
//!
//! # What is implemented
//!
//! Every rule type (`sequence`, `condition`, `block`), and, in the order the
//! task asked them to be tackled:
//!
//! * `biome` — exact match against a name or a list of names.
//! * `noise_threshold` — 2D or 3D, against any named noise in the pack's
//!   `NoiseRegistry` (so `minecraft:surface`, `minecraft:gravel`, … all work,
//!   not just the ones this file happens to import).
//! * `vertical_gradient` — a Y anchor pair plus a seeded per-block coin flip
//!   in between; this is what places bedrock (`bedrock_floor`) and the
//!   stone/deepslate boundary (`deepslate`) for free.
//! * `y_above`
//! * `water`
//! * `not`
//! * `above_preliminary_surface`
//! * `stone_depth` — both `floor` (distance below the top of the current
//!   solid run) and `ceiling` (distance above the bottom of it), computed
//!   from the same column of blocks the noise stage already produced.
//!
//! # What is skipped, and why
//!
//! `temperature`, `steep` and `hole` are read out of the tree (so a condition
//! of that type is a real, named skip, not a silent one) but always evaluate
//! to `false`. Vanilla's exact semantics for these three depend on state this
//! generator does not reconstruct with confidence — `steep` and `hole` read
//! neighbouring-column heightmaps, `temperature` reads a biome's *adjusted*
//! temperature function, not the raw climate sample — and guessing at them
//! risks silently-wrong terrain, which is the one thing this module is not
//! willing to produce. Their combined use in the overworld's tree is a small
//! minority of conditions, all in secondary placement (snow/powder-snow
//! patches, a cave variant of a badlands rule); none of them gate the primary
//! grass / dirt / sand / gravel / sandstone / deepslate / bedrock rules the
//! rest of the tree places, which behave normally regardless.
//!
//! `minecraft:bandlands` — the badlands' terracotta colour banding — is a
//! *rule*, not a condition, and is likewise read and skipped as a no-op: a
//! badlands column gets plain terracotta instead of banded colours.
//!
//! [`SurfaceRuleSet::unsupported`] lists exactly which condition/rule types a
//! loaded tree actually used and skipped, so a caller can see this without
//! reading the source.
//!
//! # The per-column walk
//!
//! Vanilla evaluates the tree once per block, scanning a column top to
//! bottom and threading state through it: how many solid blocks deep we are
//! below the last air/fluid above (`stoneDepthAbove`), the mirror image
//! counted from the bottom of the current solid run (`stoneDepthBelow`), the
//! Y of the fluid surface directly above (if any), and the biome. All of that
//! is derivable from the same noise-stage column the caller already has, so
//! [`SurfaceRuleSet::paint`] takes that column and returns the finished one.

use std::collections::HashSet;
use std::sync::Arc;

use aether_world::registry::blocks;
use aether_world::BlockStateId;

use super::density::{BuildError, NoiseRegistry};
use super::json::Json;
use super::noise::NormalNoise;
use super::random::PositionalFactory;
use super::terrain::NoiseBlock;

/// A Y anchor as `noise_settings` writes it: an absolute Y, or relative to the
/// dimension's floor or ceiling.
#[derive(Debug, Clone, Copy)]
enum Anchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
}

impl Anchor {
    fn parse(j: &Json) -> Result<Self, BuildError> {
        if let Some(v) = j.get("absolute").and_then(Json::as_f64) {
            return Ok(Anchor::Absolute(v as i32));
        }
        if let Some(v) = j.get("above_bottom").and_then(Json::as_f64) {
            return Ok(Anchor::AboveBottom(v as i32));
        }
        if let Some(v) = j.get("below_top").and_then(Json::as_f64) {
            return Ok(Anchor::BelowTop(v as i32));
        }
        Err(BuildError::new(format!(
            "surface rule: unrecognized y anchor {j:?}"
        )))
    }

    fn resolve(self, min_y: i32, height: i32) -> i32 {
        match self {
            Anchor::Absolute(v) => v,
            Anchor::AboveBottom(v) => min_y + v,
            Anchor::BelowTop(v) => min_y + height - v,
        }
    }
}

/// One leaf of `if_true`.
enum Cond {
    Biome(Vec<String>),
    NoiseThreshold {
        noise: Arc<NormalNoise>,
        min: f64,
        max: f64,
        is_3d: bool,
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
    /// A condition type read from the tree but not evaluated faithfully; see
    /// the module docs. Always false.
    Unsupported,
}

/// One node of the rule tree.
enum Rule {
    Sequence(Vec<Rule>),
    Condition(Cond, Box<Rule>),
    Block(BlockStateId),
    /// `minecraft:bandlands`, or any rule type not recognized: read, named,
    /// and skipped. See the module docs.
    NoOp,
}

fn as_bool(j: Option<&Json>, default: bool) -> bool {
    match j {
        Some(Json::Bool(b)) => *b,
        _ => default,
    }
}

fn as_i32(j: Option<&Json>, default: i32) -> i32 {
    j.and_then(Json::as_f64).map(|v| v as i32).unwrap_or(default)
}

fn rule_type(j: &Json) -> Result<&str, BuildError> {
    j.get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| BuildError::new("surface rule: node has no `type`"))
}

fn resolve_block(name: &str) -> Result<BlockStateId, BuildError> {
    blocks::default_state(name)
        .ok_or_else(|| BuildError::new(format!("surface rule: block registry has no `{name}`")))
}

struct Loader<'a> {
    noises: &'a NoiseRegistry,
    min_y: i32,
    height: i32,
    unsupported: HashSet<String>,
}

impl<'a> Loader<'a> {
    fn parse_rule(&mut self, j: &Json) -> Result<Rule, BuildError> {
        match rule_type(j)? {
            "minecraft:sequence" => {
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
            "minecraft:condition" => {
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
            "minecraft:block" => {
                let name = j
                    .get("result_state")
                    .and_then(|s| s.get("Name"))
                    .and_then(Json::as_str)
                    .ok_or_else(|| BuildError::new("block rule: no `result_state.Name`"))?;
                // Content packs can name blocks a given build of the engine's
                // registry does not have yet (an experimental / newer-version
                // block, say). That is a real gap, not something to paper
                // over with a substitute block someone did not ask for — so
                // it is recorded the same way an unsupported condition is,
                // and the leaf falls through rather than placing the wrong
                // thing.
                match resolve_block(name) {
                    Ok(id) => Ok(Rule::Block(id)),
                    Err(_) => {
                        self.unsupported.insert(format!("block:{name}"));
                        Ok(Rule::NoOp)
                    }
                }
            }
            other => {
                self.unsupported.insert(format!("rule:{other}"));
                Ok(Rule::NoOp)
            }
        }
    }

    fn parse_cond(&mut self, j: &Json) -> Result<Cond, BuildError> {
        match rule_type(j)? {
            "minecraft:biome" => {
                let v = j
                    .get("biome_is")
                    .ok_or_else(|| BuildError::new("biome condition: no `biome_is`"))?;
                let list = match v {
                    Json::Str(s) => vec![s.clone()],
                    Json::Arr(a) => a
                        .iter()
                        .filter_map(Json::as_str)
                        .map(str::to_string)
                        .collect(),
                    _ => {
                        return Err(BuildError::new(
                            "biome condition: `biome_is` is not a string or array",
                        ))
                    }
                };
                Ok(Cond::Biome(list))
            }
            "minecraft:noise_threshold" => {
                let name = j
                    .get("noise")
                    .and_then(Json::as_str)
                    .ok_or_else(|| BuildError::new("noise_threshold: no `noise`"))?;
                let noise = self.noises.get(name)?;
                let min = j
                    .get("min_threshold")
                    .and_then(Json::as_f64)
                    .ok_or_else(|| BuildError::new("noise_threshold: no `min_threshold`"))?;
                let max = j
                    .get("max_threshold")
                    .and_then(Json::as_f64)
                    .ok_or_else(|| BuildError::new("noise_threshold: no `max_threshold`"))?;
                let is_3d = as_bool(j.get("is_3d"), false);
                Ok(Cond::NoiseThreshold {
                    noise,
                    min,
                    max,
                    is_3d,
                })
            }
            "minecraft:vertical_gradient" => {
                let true_a = Anchor::parse(j.get("true_at_and_below").ok_or_else(|| {
                    BuildError::new("vertical_gradient: no `true_at_and_below`")
                })?)?;
                let false_a = Anchor::parse(j.get("false_at_and_above").ok_or_else(|| {
                    BuildError::new("vertical_gradient: no `false_at_and_above`")
                })?)?;
                let name = j
                    .get("random_name")
                    .and_then(Json::as_str)
                    .ok_or_else(|| BuildError::new("vertical_gradient: no `random_name`"))?;
                Ok(Cond::VerticalGradient {
                    true_y: true_a.resolve(self.min_y, self.height),
                    false_y: false_a.resolve(self.min_y, self.height),
                    random: self.noises.forked_factory(name),
                })
            }
            "minecraft:y_above" => {
                let anchor = Anchor::parse(
                    j.get("anchor")
                        .ok_or_else(|| BuildError::new("y_above: no `anchor`"))?,
                )?;
                Ok(Cond::YAbove {
                    anchor_y: anchor.resolve(self.min_y, self.height),
                    surface_depth_multiplier: as_i32(j.get("surface_depth_multiplier"), 1),
                    add_stone_depth: as_bool(j.get("add_stone_depth"), true),
                })
            }
            "minecraft:water" => Ok(Cond::Water {
                offset: as_i32(j.get("offset"), 0),
                surface_depth_multiplier: as_i32(j.get("surface_depth_multiplier"), 0),
                add_stone_depth: as_bool(j.get("add_stone_depth"), false),
            }),
            "minecraft:not" => {
                let inner = j
                    .get("invert")
                    .ok_or_else(|| BuildError::new("not condition: no `invert`"))?;
                Ok(Cond::Not(Box::new(self.parse_cond(inner)?)))
            }
            "minecraft:above_preliminary_surface" => Ok(Cond::AbovePreliminarySurface),
            "minecraft:stone_depth" => Ok(Cond::StoneDepth {
                ceiling: j.get("surface_type").and_then(Json::as_str) == Some("ceiling"),
                add_surface_depth: as_bool(j.get("add_surface_depth"), false),
                offset: as_i32(j.get("offset"), 0),
                secondary_depth_range: as_i32(j.get("secondary_depth_range"), 0),
            }),
            other => {
                self.unsupported.insert(other.to_string());
                Ok(Cond::Unsupported)
            }
        }
    }
}

/// A loaded, ready-to-evaluate surface rule tree.
pub struct SurfaceRuleSet {
    root: Rule,
    surface_noise: Arc<NormalNoise>,
    surface_secondary_noise: Arc<NormalNoise>,
    min_y: i32,
    height: i32,
    unsupported: Vec<String>,
}

/// A biome lookup, quart-resolution — however the caller wants to provide it.
/// [`super::overworld::OverworldBiomeSource::biome_at`] has exactly this
/// shape.
pub trait BiomeAt {
    fn biome_at(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> &str;
}

impl SurfaceRuleSet {
    /// Build from the `surface_rule` object out of a dimension's
    /// `noise_settings` file.
    pub fn load(
        surface_rule: &Json,
        noises: &NoiseRegistry,
        min_y: i32,
        height: i32,
    ) -> Result<Self, BuildError> {
        let mut loader = Loader {
            noises,
            min_y,
            height,
            unsupported: HashSet::new(),
        };
        let root = loader.parse_rule(surface_rule)?;
        let mut unsupported: Vec<String> = loader.unsupported.into_iter().collect();
        unsupported.sort();
        Ok(Self {
            root,
            surface_noise: noises.get("minecraft:surface")?,
            surface_secondary_noise: noises.get("minecraft:surface_secondary")?,
            min_y,
            height,
            unsupported,
        })
    }

    /// The condition and rule types this tree used but does not evaluate
    /// faithfully — see the module docs. Empty means the tree that was
    /// loaded is fully covered.
    pub fn unsupported(&self) -> &[String] {
        &self.unsupported
    }

    /// Paint one column of noise-stage blocks in place.
    ///
    /// `raw` is `min_y..min_y+height`, bottom to top — exactly
    /// [`super::terrain::ChunkTerrain::column`]'s output. `biome_at` supplies
    /// the biome at a quart position (see [`BiomeAt`]); `preliminary_surface`
    /// is `Terrain::preliminary_surface_level(x, z)`.
    pub fn paint(
        &self,
        x: i32,
        z: i32,
        raw: &mut [NoiseBlock],
        block_of: impl Fn(NoiseBlock) -> BlockStateId,
        biome_at: &impl BiomeAt,
        preliminary_surface: i32,
    ) -> Vec<BlockStateId> {
        let height = raw.len();
        debug_assert_eq!(height, self.height as usize);

        let solid: Vec<bool> = raw
            .iter()
            .map(|b| !matches!(b, NoiseBlock::Air | NoiseBlock::Water | NoiseBlock::Lava))
            .collect();

        // stoneDepthAbove: consecutive solid cells since the last air/fluid,
        // walking from the top down (index = height-1 -> 0).
        let mut stone_depth_above = vec![0i32; height];
        {
            let mut d = 0i32;
            for i in (0..height).rev() {
                d = if solid[i] { d + 1 } else { 0 };
                stone_depth_above[i] = d;
            }
        }
        // stoneDepthBelow: the mirror image, walking bottom up.
        let mut stone_depth_below = vec![0i32; height];
        {
            let mut d = 0i32;
            for i in 0..height {
                d = if solid[i] { d + 1 } else { 0 };
                stone_depth_below[i] = d;
            }
        }
        // fluid_top: the absolute Y of the surface of the nearest fluid body
        // above, uninterrupted by air since. None once air has reset it.
        let mut fluid_top = vec![None; height];
        {
            let mut top: Option<i32> = None;
            for i in (0..height).rev() {
                match raw[i] {
                    NoiseBlock::Air => top = None,
                    NoiseBlock::Water | NoiseBlock::Lava => {
                        if top.is_none() {
                            top = Some(self.min_y + i as i32 + 1);
                        }
                    }
                    _ => {}
                }
                fluid_top[i] = top;
            }
        }

        let surface_depth =
            (self.surface_noise.get_value(x as f64, 0.0, z as f64) * 2.75 + 3.0) as i32;
        let surface_secondary = self.surface_secondary_noise.get_value(x as f64, 0.0, z as f64);

        let mut out = Vec::with_capacity(height);
        let mut cached_biome: Option<(i32, i32, i32, String)> = None;
        for i in 0..height {
            let y = self.min_y + i as i32;
            if !solid[i] {
                out.push(block_of(raw[i]));
                continue;
            }
            let qx = x >> 2;
            let qy = y >> 2;
            let qz = z >> 2;
            let biome: &str = match &cached_biome {
                Some((cx, cy, cz, b)) if *cx == qx && *cy == qy && *cz == qz => b.as_str(),
                _ => {
                    let b = biome_at.biome_at(qx, qy, qz).to_string();
                    cached_biome = Some((qx, qy, qz, b));
                    cached_biome.as_ref().unwrap().3.as_str()
                }
            };
            let ctx = EvalCtx {
                x,
                y,
                z,
                biome,
                surface_depth,
                surface_secondary,
                stone_depth_above: stone_depth_above[i],
                stone_depth_below: stone_depth_below[i],
                fluid_top: fluid_top[i],
                above_preliminary_surface: y >= preliminary_surface,
            };
            match eval_rule(&self.root, &ctx) {
                Some(id) => out.push(id),
                None => out.push(block_of(raw[i])),
            }
        }
        out
    }
}

struct EvalCtx<'a> {
    x: i32,
    y: i32,
    z: i32,
    biome: &'a str,
    surface_depth: i32,
    surface_secondary: f64,
    stone_depth_above: i32,
    stone_depth_below: i32,
    fluid_top: Option<i32>,
    above_preliminary_surface: bool,
}

fn eval_rule(rule: &Rule, ctx: &EvalCtx) -> Option<BlockStateId> {
    match rule {
        Rule::Sequence(seq) => seq.iter().find_map(|r| eval_rule(r, ctx)),
        Rule::Condition(c, then) => {
            if eval_cond(c, ctx) {
                eval_rule(then, ctx)
            } else {
                None
            }
        }
        Rule::Block(id) => Some(*id),
        Rule::NoOp => None,
    }
}

fn eval_cond(cond: &Cond, ctx: &EvalCtx) -> bool {
    match cond {
        Cond::Biome(list) => list.iter().any(|b| b == ctx.biome),
        Cond::NoiseThreshold {
            noise,
            min,
            max,
            is_3d,
        } => {
            let v = if *is_3d {
                noise.get_value(ctx.x as f64, ctx.y as f64, ctx.z as f64)
            } else {
                noise.get_value(ctx.x as f64, 0.0, ctx.z as f64)
            };
            v >= *min && v <= *max
        }
        Cond::VerticalGradient {
            true_y,
            false_y,
            random,
        } => {
            if ctx.y <= *true_y {
                true
            } else if ctx.y >= *false_y {
                false
            } else {
                let d = 1.0 - (ctx.y - true_y) as f64 / (false_y - true_y) as f64;
                let mut r = random.at(ctx.x, ctx.y, ctx.z);
                (r.next_f32() as f64) < d
            }
        }
        Cond::YAbove {
            anchor_y,
            surface_depth_multiplier,
            add_stone_depth,
        } => {
            let adj = if *add_stone_depth { ctx.stone_depth_above } else { 0 };
            ctx.y + adj >= anchor_y + ctx.surface_depth * surface_depth_multiplier
        }
        Cond::Water {
            offset,
            surface_depth_multiplier,
            add_stone_depth,
        } => match ctx.fluid_top {
            None => true,
            Some(top) => {
                let adj = if *add_stone_depth { ctx.stone_depth_above } else { 0 };
                ctx.y + adj >= top + offset + ctx.surface_depth * surface_depth_multiplier
            }
        },
        Cond::Not(inner) => !eval_cond(inner, ctx),
        Cond::AbovePreliminarySurface => ctx.above_preliminary_surface,
        Cond::StoneDepth {
            ceiling,
            add_surface_depth,
            offset,
            secondary_depth_range,
        } => {
            let i = if *ceiling { ctx.stone_depth_below } else { ctx.stone_depth_above };
            let j = if *add_surface_depth { ctx.surface_depth } else { 0 };
            let k = if *secondary_depth_range == 0 {
                0
            } else {
                (((ctx.surface_secondary + 1.0) / 2.0) * *secondary_depth_range as f64) as i32
            };
            i <= 1 + offset + j + k
        }
        Cond::Unsupported => false,
    }
}
