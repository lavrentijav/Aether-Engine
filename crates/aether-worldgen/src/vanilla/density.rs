//! The data-driven density-function graph.
//!
//! Since 1.18 vanilla's terrain and climate are not an algorithm with constants
//! baked into the code; they are a *graph* of small operators whose definitions
//! live in the game's `data/minecraft/worldgen/` tree. This module reads that
//! tree at run time and evaluates the graph. Nothing about the overworld's
//! shape is hard-coded here — only the operator semantics are.
//!
//! # What is and isn't implemented
//!
//! Every operator the overworld *climate* router needs is implemented and
//! measured (see the crate docs for the parity numbers). The operators that
//! only the full terrain density needs are implemented where their semantics
//! are unambiguous, with one exception that is called out at its definition:
//!
//! * [`Node::Interpolated`] evaluates its argument directly instead of
//!   reproducing vanilla's cell-grid interpolation. Correct for single-point
//!   climate sampling, **wrong** for block-by-block terrain.
//!
//! Blending (`blend_alpha` / `blend_offset` / `blend_density`) is the identity
//! here, matching vanilla's `Blender.empty()` — blending only ever engages at
//! the seam with an old, differently-generated region.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::json::Json;
use super::noise::NormalNoise;
use super::random::XoroshiroRandom;

/// Where a density function is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx {
    /// Block X.
    pub x: i32,
    /// Block Y.
    pub y: i32,
    /// Block Z.
    pub z: i32,
}

impl Ctx {
    /// A context at one block position.
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

/// Something went wrong loading or building the graph.
#[derive(Debug, Clone)]
pub struct BuildError(String);

impl BuildError {
    /// A build failure with a human-readable explanation.
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BuildError {}

fn err<T>(msg: impl Into<String>) -> Result<T, BuildError> {
    Err(BuildError(msg.into()))
}

// --- data pack -------------------------------------------------------------

/// A read-only view of the game's own worldgen data.
///
/// Point this at a directory holding `data/minecraft/worldgen/...` — either an
/// unpacked server jar or a data pack. The files are read, never written, and
/// never copied into this repository.
#[derive(Debug, Clone)]
pub struct DataPack {
    worldgen: PathBuf,
}

impl DataPack {
    /// Open a pack rooted at `root`.
    ///
    /// Accepts either the pack root (containing `data/`) or the
    /// `data/minecraft/worldgen` directory itself, because both are things an
    /// operator plausibly has on disk.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, BuildError> {
        let root = root.as_ref();
        let candidates = [root.join("data/minecraft/worldgen"), root.to_path_buf()];
        for c in candidates {
            if c.join("noise_settings").is_dir() && c.join("density_function").is_dir() {
                return Ok(Self { worldgen: c });
            }
        }
        err(format!(
            "{} does not look like a worldgen data pack (no data/minecraft/worldgen/{{noise_settings,density_function}})",
            root.display()
        ))
    }

    fn read(&self, kind: &str, id: &str) -> Result<Json, BuildError> {
        let path = self
            .worldgen
            .join(kind)
            .join(format!("{}.json", strip_ns(id)));
        let text = std::fs::read_to_string(&path)
            .map_err(|e| BuildError(format!("cannot read {}: {e}", path.display())))?;
        Json::parse(&text).map_err(|e| BuildError(format!("{}: {e}", path.display())))
    }

    /// The raw JSON of `worldgen/<kind>/<id>.json` — `kind` is a registry
    /// directory such as `biome`, `placed_feature` or `configured_carver`.
    pub fn read_json(&self, kind: &str, id: &str) -> Result<Json, BuildError> {
        self.read(kind, id)
    }

    /// Every id in `worldgen/<kind>/`, namespaced and sorted. Subdirectories
    /// become path segments (`minecraft:trees/oak`).
    pub fn list(&self, kind: &str) -> Result<Vec<String>, BuildError> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> std::io::Result<()> {
            for e in std::fs::read_dir(dir)? {
                let e = e?;
                let name = e.file_name().to_string_lossy().into_owned();
                let p = e.path();
                if p.is_dir() {
                    walk(&p, &format!("{prefix}{name}/"), out)?;
                } else if let Some(stem) = name.strip_suffix(".json") {
                    out.push(format!("minecraft:{prefix}{stem}"));
                }
            }
            Ok(())
        }
        let dir = self.worldgen.join(kind);
        let mut out = Vec::new();
        walk(&dir, "", &mut out)
            .map_err(|e| BuildError(format!("cannot list {}: {e}", dir.display())))?;
        out.sort();
        Ok(out)
    }

    /// The raw JSON of `data/minecraft/tags/<kind>/<id>.json` (`kind` is
    /// e.g. `block`), or `None` when the pack has no such tag.
    pub fn tag_json(&self, kind: &str, id: &str) -> Option<Json> {
        let path = self
            .worldgen
            .parent()?
            .join("tags")
            .join(kind)
            .join(format!("{}.json", strip_ns(id)));
        let text = std::fs::read_to_string(path).ok()?;
        Json::parse(&text).ok()
    }

    /// The raw JSON of one `worldgen/noise_settings/<id>.json`.
    pub fn noise_settings(&self, id: &str) -> Result<Json, BuildError> {
        self.read("noise_settings", id)
    }

    /// The raw JSON of one `worldgen/density_function/<id>.json`.
    pub fn density_function(&self, id: &str) -> Result<Json, BuildError> {
        self.read("density_function", id)
    }

    /// The raw JSON of one `worldgen/noise/<id>.json`.
    pub fn noise_parameters(&self, id: &str) -> Result<Json, BuildError> {
        self.read("noise", id)
    }
}

/// `minecraft:foo/bar` → `foo/bar`. Unnamespaced ids are already relative.
fn strip_ns(id: &str) -> &str {
    id.split_once(':').map(|(_, rest)| rest).unwrap_or(id)
}

/// `foo` → `minecraft:foo`, leaving already-namespaced ids alone. The
/// namespaced form is what vanilla hashes to seed a noise, so it has to be
/// exact.
fn with_ns(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

// --- noises ----------------------------------------------------------------

/// The named [`NormalNoise`] instances a graph refers to, seeded for one world.
///
/// Each noise is seeded from the MD5 of its *namespaced id* against the world
/// seed's positional factory, so a noise's stream depends only on its name —
/// never on how many other noises exist or the order they were built in.
#[derive(Debug)]
pub struct NoiseRegistry {
    pack: DataPack,
    factory: super::random::PositionalFactory,
    cache: std::sync::Mutex<HashMap<String, Arc<NormalNoise>>>,
}

impl NoiseRegistry {
    /// Build a registry for `seed`.
    pub fn new(pack: DataPack, seed: u64) -> Self {
        let mut root = XoroshiroRandom::new(seed);
        Self {
            pack,
            factory: root.fork_positional(),
            cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// A named positional factory forked off the world seed — vanilla's
    /// `randomFactory.fromHashOf(name).forkPositional()`, which is how the
    /// aquifer and the ore veins get their per-position streams.
    pub fn forked_factory(&self, name: &str) -> super::random::PositionalFactory {
        let mut r = self.factory.from_hash_of(name);
        r.fork_positional()
    }

    /// The world's root positional factory — vanilla's `RandomState.random`,
    /// which the surface system draws its per-column randoms from.
    pub fn factory(&self) -> super::random::PositionalFactory {
        self.factory
    }

    /// The old terrain noise, seeded from `minecraft:terrain`.
    ///
    /// It is built fresh per call rather than cached by name because its scale
    /// parameters come from the call site rather than from a data file — but
    /// the overworld only ever asks for one of them.
    pub fn blended_terrain(
        &self,
        xz_scale: f64,
        y_scale: f64,
        xz_factor: f64,
        y_factor: f64,
        smear_scale_multiplier: f64,
    ) -> Arc<super::noise::BlendedNoise> {
        let mut r = self.factory.from_hash_of("minecraft:terrain");
        Arc::new(super::noise::BlendedNoise::new(
            &mut r,
            xz_scale,
            y_scale,
            xz_factor,
            y_factor,
            smear_scale_multiplier,
        ))
    }

    /// The noise named `id` (`minecraft:continentalness`, …), built on first
    /// use and shared afterwards.
    pub fn get(&self, id: &str) -> Result<Arc<NormalNoise>, BuildError> {
        let key = with_ns(id);
        if let Some(n) = self.cache.lock().unwrap().get(&key) {
            return Ok(Arc::clone(n));
        }
        let params = self.pack.noise_parameters(&key)?;
        let first_octave = params
            .get("firstOctave")
            .and_then(Json::as_f64)
            .ok_or_else(|| BuildError(format!("{key}: no firstOctave")))?
            as i32;
        let amps: Vec<f64> = params
            .get("amplitudes")
            .and_then(Json::as_arr)
            .ok_or_else(|| BuildError(format!("{key}: no amplitudes")))?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0))
            .collect();
        let mut r = self.factory.from_hash_of(&key);
        let noise = Arc::new(NormalNoise::create(&mut r, first_octave, &amps));
        self.cache.lock().unwrap().insert(key, Arc::clone(&noise));
        Ok(noise)
    }
}

// --- splines ---------------------------------------------------------------

/// A cubic spline over another density function.
///
/// Everything here is `f32`, not `f64`: vanilla's splines are float-typed all
/// the way through, and the terrain offset spline is deep enough that widening
/// it to double changes the result.
#[derive(Debug)]
pub struct Spline {
    coordinate: Arc<Node>,
    locations: Vec<f32>,
    values: Vec<SplineValue>,
    derivatives: Vec<f32>,
}

#[derive(Debug)]
enum SplineValue {
    Const(f32),
    Nested(Arc<Spline>),
}

impl SplineValue {
    fn apply(&self, ctx: Ctx) -> f32 {
        match self {
            SplineValue::Const(v) => *v,
            SplineValue::Nested(s) => s.apply(ctx),
        }
    }
}

impl Spline {
    /// Evaluate at `ctx`.
    pub fn apply(&self, ctx: Ctx) -> f32 {
        let point = self.coordinate.compute(ctx) as f32;
        // The last knot at or below `point`; -1 when `point` sits left of all
        // of them.
        let i = match self.locations.iter().position(|&l| point < l) {
            Some(k) => k as isize - 1,
            None => self.locations.len() as isize - 1,
        };
        let last = self.locations.len() as isize - 1;
        if i < 0 {
            return self.linear_extend(point, self.values[0].apply(ctx), 0);
        }
        if i == last {
            let k = last as usize;
            return self.linear_extend(point, self.values[k].apply(ctx), k);
        }
        let k = i as usize;
        let lo = self.locations[k];
        let hi = self.locations[k + 1];
        let t = (point - lo) / (hi - lo);
        let a = self.values[k].apply(ctx);
        let b = self.values[k + 1].apply(ctx);
        // Hermite in disguise: `n` and `o` are how far each end's declared
        // slope overshoots the straight line between the knots.
        let n = self.derivatives[k] * (hi - lo) - (b - a);
        let o = -self.derivatives[k + 1] * (hi - lo) + (b - a);
        lerp_f32(t, a, b) + t * (1.0 - t) * lerp_f32(t, n, o)
    }

    /// Outside the knot range vanilla extends along the end slope — or holds
    /// the end value flat when that slope is exactly zero.
    fn linear_extend(&self, point: f32, value: f32, i: usize) -> f32 {
        let d = self.derivatives[i];
        if d == 0.0 {
            value
        } else {
            value + d * (point - self.locations[i])
        }
    }
}

#[inline]
fn lerp_f32(t: f32, a: f32, b: f32) -> f32 {
    a + t * (b - a)
}

// --- the graph -------------------------------------------------------------

/// One node of the density-function graph.
#[derive(Debug)]
pub enum Node {
    /// A literal.
    Const(f64),
    /// `a + b`.
    Add(Arc<Node>, Arc<Node>),
    /// `a * b`.
    Mul(Arc<Node>, Arc<Node>),
    /// `min(a, b)`.
    Min(Arc<Node>, Arc<Node>),
    /// `max(a, b)`.
    Max(Arc<Node>, Arc<Node>),
    /// `|a|`.
    Abs(Arc<Node>),
    /// `a²`.
    Square(Arc<Node>),
    /// `a³`.
    Cube(Arc<Node>),
    /// `a` above zero, `a/2` below — an asymmetric squash.
    HalfNegative(Arc<Node>),
    /// `a` above zero, `a/4` below.
    QuarterNegative(Arc<Node>),
    /// Clamp to `[-1, 1]`, then `x/2 − x³/24`.
    Squeeze(Arc<Node>),
    /// `1 / a`.
    Invert(Arc<Node>),
    /// A named noise sampled at scaled coordinates.
    Noise {
        /// The noise.
        noise: Arc<NormalNoise>,
        /// Scale applied to X and Z.
        xz_scale: f64,
        /// Scale applied to Y.
        y_scale: f64,
    },
    /// A named noise whose sample point is displaced by three other functions.
    ShiftedNoise {
        /// The noise.
        noise: Arc<NormalNoise>,
        /// X displacement.
        shift_x: Arc<Node>,
        /// Y displacement.
        shift_y: Arc<Node>,
        /// Z displacement.
        shift_z: Arc<Node>,
        /// Scale applied to X and Z before displacement.
        xz_scale: f64,
        /// Scale applied to Y before displacement.
        y_scale: f64,
    },
    /// `offset(x, 0, z) * 4`, the X half of the climate shift pair.
    ShiftA(Arc<NormalNoise>),
    /// `offset(z, x, 0) * 4`, the Z half. The argument order really is
    /// rotated like that.
    ShiftB(Arc<NormalNoise>),
    /// `offset(x, y, z) * 4` — the 3D shift.
    Shift(Arc<NormalNoise>),
    /// A linear ramp in Y, clamped outside its band.
    YClampedGradient {
        /// Y at which the ramp starts.
        from_y: f64,
        /// Y at which it ends.
        to_y: f64,
        /// Value at `from_y`.
        from_value: f64,
        /// Value at `to_y`.
        to_value: f64,
    },
    /// Pick one of two functions by whether `input` lands in `[min, max)`.
    RangeChoice {
        /// The value being tested.
        input: Arc<Node>,
        /// Inclusive lower bound.
        min_inclusive: f64,
        /// Exclusive upper bound.
        max_exclusive: f64,
        /// Evaluated when in range.
        in_range: Arc<Node>,
        /// Evaluated otherwise.
        out_of_range: Arc<Node>,
    },
    /// Pick from `functions` by which threshold band `input` falls in.
    ///
    /// The bands are half-open, `[t, t')`. This operator only appears inside
    /// the cave functions, which `final_density` reaches — so the router parity
    /// measurement does exercise it.
    IntervalSelect {
        /// The value being tested.
        input: Arc<Node>,
        /// Sorted band edges; `functions.len() - 1` of them.
        thresholds: Vec<f64>,
        /// One function per band.
        functions: Vec<Arc<Node>>,
    },
    /// Clamp to a fixed range.
    Clamp {
        /// The value being clamped.
        input: Arc<Node>,
        /// Lower bound.
        min: f64,
        /// Upper bound.
        max: f64,
    },
    /// A cubic spline.
    SplineNode(Arc<Spline>),
    /// `cache_once` / `cache_all_in_cell`: real memoization keyed on the exact
    /// block position, matching vanilla's single-value caches. The value is
    /// unchanged from evaluating `inner` directly — this is pure memoization
    /// of a pure function — but without it, a subtree referenced from several
    /// places in the graph (very common: `erosion`, `continentalness`, …) gets
    /// recomputed once per reference, and those references nest, so the cost
    /// compounds. See [`marker_cache`].
    CacheOnce {
        /// The cached function.
        inner: Arc<Node>,
        /// Identifies this node in the marker cache. Assigned at build time.
        id: u32,
    },
    /// `cache_2d` / `flat_cache`: memoization keyed on `(x, z)` only, `y`
    /// dropped — correct because these wrap functions vanilla itself declares
    /// column-only (the 2D climate functions: continentalness, erosion, …).
    /// This is where most of the column-scan win lives: a `(x, z)`-keyed
    /// cache stays hot for the whole 384-block Y loop at one column, turning
    /// an O(height) cost into O(1).
    ///
    /// `flat_cache` deserves a further note: as a *density function* it is
    /// transparent, which is what the game's own noise router evaluates to.
    /// Inside a generating chunk vanilla swaps it for a per-quart-column cache
    /// that quantizes X and Z down to the quart origin — so the two can differ
    /// at positions that are not quart-aligned. This cache does not quantize,
    /// so it still computes the exact value at each `(x, z)`; it only avoids
    /// recomputing that exact value on a repeat visit. See
    /// [`Node::Interpolated`]'s sibling note — the quantizing behavior is a
    /// separate, pre-existing gap, not something this cache changes either
    /// way.
    Cache2D {
        /// The cached function.
        inner: Arc<Node>,
        /// Identifies this node in the marker cache. Assigned at build time.
        id: u32,
    },
    /// `minecraft:interpolated` in its *router* form, where it is transparent.
    ///
    /// This is not a shortcut: the game's own `NoiseRouter` evaluates it
    /// exactly like this. The interpolating form below only exists inside a
    /// generating chunk, where vanilla swaps it in.
    Interpolated(Arc<Node>),
    /// `minecraft:interpolated` in its *chunk* form: the argument is evaluated
    /// only at the corners of a `cell_width × cell_height × cell_width` cell
    /// and everything inside the cell is trilinearly interpolated.
    ///
    /// This is the single largest saving in vanilla's terrain pipeline — the
    /// expensive noise runs once per 4×8×4 block of world instead of once per
    /// block — and it is *visible*, not an optimization: the terrain vanilla
    /// generates is the interpolated field, not the true one.
    ///
    /// The lattice is fixed in world space (`floorDiv` of the position by the
    /// cell size), not relative to the chunk, so a cell straddling a chunk
    /// border gives the same answer from either side and the value here is a
    /// pure function of the position.
    CellInterpolated {
        /// The function sampled at the cell corners.
        inner: Arc<Node>,
        /// Cell size on X and Z, in blocks.
        cell_width: i32,
        /// Cell size on Y, in blocks.
        cell_height: i32,
        /// Identifies this node in the corner cache. Assigned at build time so
        /// two different interpolated nodes never share a cache line.
        id: u32,
    },
    /// Blend weight; `1.0` without an old-region seam.
    BlendAlpha,
    /// Blend offset; `0.0` without an old-region seam.
    BlendOffset,
    /// Blended density; the identity without an old-region seam.
    BlendDensity(Arc<Node>),
    /// Scan downwards from an estimated ceiling for the first cell whose
    /// density is solid, and report that Y — vanilla's rough surface estimate,
    /// used to decide what "near the surface" means before the surface itself
    /// exists.
    FindTopSurface {
        /// The density probed at each step.
        density: Arc<Node>,
        /// Where to start, before rounding down to a cell boundary.
        upper_bound: Arc<Node>,
        /// The Y reported when nothing solid is found.
        lower_bound: i32,
        /// The step, in blocks.
        cell_height: i32,
    },
    /// `minecraft:old_blended_noise` — the pre-1.18 terrain noise, which still
    /// supplies the overworld's 3D shape.
    OldBlendedNoise(Arc<super::noise::BlendedNoise>),
    /// `minecraft:weird_scaled_sampler`: the spaghetti caves' rarity-scaled
    /// noise. `input` is quantized to a rarity `r` and the result is
    /// `r * |noise(pos / r)|`.
    WeirdScaledSampler {
        /// The rarity input.
        input: Arc<Node>,
        /// The sampled noise.
        noise: Arc<NormalNoise>,
        /// `type_1` (3D rarity) when true, `type_2` (2D rarity) otherwise.
        type_1: bool,
    },
}

/// `NoiseRouterData.QuantizedSpaghettiRarity.getSpaghettiRarity3D`.
fn spaghetti_rarity_3d(v: f64) -> f64 {
    if v < -0.5 {
        0.75
    } else if v < 0.0 {
        1.0
    } else if v < 0.5 {
        1.5
    } else {
        2.0
    }
}

/// `NoiseRouterData.QuantizedSpaghettiRarity.getSphaghettiRarity2D`.
fn spaghetti_rarity_2d(v: f64) -> f64 {
    if v < -0.75 {
        0.5
    } else if v < -0.5 {
        0.75
    } else if v < 0.5 {
        1.0
    } else if v < 0.75 {
        2.0
    } else {
        3.0
    }
}

impl Node {
    /// Evaluate the graph at `ctx`.
    pub fn compute(&self, ctx: Ctx) -> f64 {
        match self {
            Node::Const(v) => *v,
            Node::Add(a, b) => a.compute(ctx) + b.compute(ctx),
            Node::Mul(a, b) => {
                let l = a.compute(ctx);
                // Vanilla short-circuits here; keeping the short-circuit keeps
                // `0 * NaN` at zero, which the cave functions rely on.
                if l == 0.0 {
                    0.0
                } else {
                    l * b.compute(ctx)
                }
            }
            Node::Min(a, b) => a.compute(ctx).min(b.compute(ctx)),
            Node::Max(a, b) => a.compute(ctx).max(b.compute(ctx)),
            Node::Abs(a) => a.compute(ctx).abs(),
            Node::Square(a) => {
                let v = a.compute(ctx);
                v * v
            }
            Node::Cube(a) => {
                let v = a.compute(ctx);
                v * v * v
            }
            Node::HalfNegative(a) => {
                let v = a.compute(ctx);
                if v > 0.0 {
                    v
                } else {
                    v * 0.5
                }
            }
            Node::QuarterNegative(a) => {
                let v = a.compute(ctx);
                if v > 0.0 {
                    v
                } else {
                    v * 0.25
                }
            }
            Node::Squeeze(a) => {
                let v = a.compute(ctx).clamp(-1.0, 1.0);
                v / 2.0 - v * v * v / 24.0
            }
            Node::Invert(a) => 1.0 / a.compute(ctx),
            Node::Noise {
                noise,
                xz_scale,
                y_scale,
            } => noise.get_value(
                ctx.x as f64 * xz_scale,
                ctx.y as f64 * y_scale,
                ctx.z as f64 * xz_scale,
            ),
            Node::ShiftedNoise {
                noise,
                shift_x,
                shift_y,
                shift_z,
                xz_scale,
                y_scale,
            } => {
                let x = ctx.x as f64 * xz_scale + shift_x.compute(ctx);
                let y = ctx.y as f64 * y_scale + shift_y.compute(ctx);
                let z = ctx.z as f64 * xz_scale + shift_z.compute(ctx);
                noise.get_value(x, y, z)
            }
            Node::ShiftA(n) => n.get_value(ctx.x as f64 * 0.25, 0.0, ctx.z as f64 * 0.25) * 4.0,
            Node::ShiftB(n) => n.get_value(ctx.z as f64 * 0.25, ctx.x as f64 * 0.25, 0.0) * 4.0,
            Node::Shift(n) => {
                n.get_value(
                    ctx.x as f64 * 0.25,
                    ctx.y as f64 * 0.25,
                    ctx.z as f64 * 0.25,
                ) * 4.0
            }
            Node::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => clamped_map(ctx.y as f64, *from_y, *to_y, *from_value, *to_value),
            Node::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                in_range,
                out_of_range,
            } => {
                let v = input.compute(ctx);
                if v >= *min_inclusive && v < *max_exclusive {
                    in_range.compute(ctx)
                } else {
                    out_of_range.compute(ctx)
                }
            }
            Node::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let v = input.compute(ctx);
                let i = thresholds.iter().take_while(|&&t| v >= t).count();
                functions[i.min(functions.len() - 1)].compute(ctx)
            }
            Node::Clamp { input, min, max } => input.compute(ctx).clamp(*min, *max),
            Node::SplineNode(s) => s.apply(ctx) as f64,
            Node::Interpolated(a) | Node::BlendDensity(a) => a.compute(ctx),
            Node::CacheOnce { inner, id } => {
                marker_cache(*id, ctx.x, ctx.y, ctx.z, || inner.compute(ctx))
            }
            Node::Cache2D { inner, id } => {
                // Y dropped from the key on purpose — see the type's docs.
                marker_cache(*id, ctx.x, 0, ctx.z, || inner.compute(ctx))
            }
            Node::CellInterpolated {
                inner,
                cell_width,
                cell_height,
                id,
            } => {
                let (w, h) = (*cell_width, *cell_height);
                let x0 = ctx.x.div_euclid(w) * w;
                let y0 = ctx.y.div_euclid(h) * h;
                let z0 = ctx.z.div_euclid(w) * w;
                let fx = (ctx.x - x0) as f64 / w as f64;
                let fy = (ctx.y - y0) as f64 / h as f64;
                let fz = (ctx.z - z0) as f64 / w as f64;
                let corners = corners_of(*id, inner, x0, y0, z0, w, h);
                // Named the way vanilla names them: noise{X}{Y}{Z}.
                let [n000, n001, n010, n011, n100, n101, n110, n111] = corners;
                // Vanilla's `Mth.lerp3`: X first, then Y, then Z. Not an
                // arbitrary choice on our part — the game has *two* orders for
                // this, and they give different last bits. `updateForY/X/Z`
                // walks Y, X, Z; the cell fill that actually places blocks goes
                // through `lerp3`, which walks X, Y, Z. This is the second one.
                let y0v = lerp(fy, lerp(fx, n000, n100), lerp(fx, n010, n110));
                let y1v = lerp(fy, lerp(fx, n001, n101), lerp(fx, n011, n111));
                lerp(fz, y0v, y1v)
            }
            Node::BlendAlpha => 1.0,
            Node::BlendOffset => 0.0,
            Node::OldBlendedNoise(n) => n.compute(ctx.x, ctx.y, ctx.z),
            Node::WeirdScaledSampler {
                input,
                noise,
                type_1,
            } => {
                let v = input.compute(ctx);
                let r = if *type_1 {
                    spaghetti_rarity_3d(v)
                } else {
                    spaghetti_rarity_2d(v)
                };
                r * noise
                    .get_value(ctx.x as f64 / r, ctx.y as f64 / r, ctx.z as f64 / r)
                    .abs()
            }
            Node::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                // The ceiling is snapped *down* to a cell boundary first, so
                // the scan only ever probes cell corners.
                let start =
                    (upper_bound.compute(ctx) / *cell_height as f64).floor() as i32 * cell_height;
                if start <= *lower_bound {
                    return *lower_bound as f64;
                }
                let mut y = start;
                while y >= *lower_bound {
                    if density.compute(Ctx::new(ctx.x, y, ctx.z)) > 0.0 {
                        return y as f64;
                    }
                    y -= cell_height;
                }
                *lower_bound as f64
            }
        }
    }
}

/// `Mth.lerp(delta, start, end)`.
#[inline]
fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// How many cells the corner cache remembers per thread.
///
/// Generation walks a column at a time, so the eight blocks stacked inside one
/// cell all want the same corners; a handful of entries turns eight evaluations
/// of a very expensive subtree into one. Sized past the number of distinct
/// `CellInterpolated` nodes that are typically live for one column (several:
/// `final_density`'s own top node plus, e.g., the noodle cave functions
/// nested inside it) rather than fixed at one, so two hot nodes don't evict
/// each other.
///
/// The working set is bigger than it looks: for one column (fixed `x`/`z`),
/// `x0`/`z0` are constant but `y0` sweeps every `cell_height`-sized bucket in
/// the column — 384/8 = 48 buckets for the overworld's 8-block cells — and
/// the overworld's graph has around 8 distinct `CellInterpolated` ids live at
/// once (the terrain shape plus several noodle-cave wrappers). That is
/// ~8 × 48 ≈ 384 *simultaneously live* corner sets for one column scan; with
/// only 64 direct-mapped slots almost every one of them evicted another
/// before it could be reused, turning the cache back into a source of
/// constant misses (and the occasional hash collision on top). Sized well
/// past that working set, with slack for hash collisions since this is
/// direct-mapped rather than associative, so a column's entries all stay
/// resident for the whole 384-block sweep.
const CORNER_CACHE_SLOTS: usize = 8192;

/// One corner-cache entry: the full key, not a packed hash of it. A packed
/// `(id, x0, y0, z0) -> u64` key that shifts each field into a fixed bit range
/// only works if the ranges don't overlap — and for `y0` (which can be
/// negative, sign-extends to a mostly-1s `u32`, and this world's `min_y` is
/// `-64`) they did, colliding with both `x0`'s and `z0`'s bits and turning
/// most of a column scan into cache misses. Storing the real tuple and using
/// the hash only to pick a slot avoids that: a collision can cost a miss, but
/// never a wrong answer.
#[derive(Clone, Copy)]
struct CornerEntry {
    id: u32,
    x0: i32,
    y0: i32,
    z0: i32,
    corners: [f64; 8],
    occupied: bool,
}

const EMPTY_CORNER_ENTRY: CornerEntry = CornerEntry {
    id: u32::MAX,
    x0: 0,
    y0: 0,
    z0: 0,
    corners: [0.0; 8],
    occupied: false,
};

thread_local! {
    static CORNER_CACHE: std::cell::RefCell<Vec<CornerEntry>> =
        std::cell::RefCell::new(vec![EMPTY_CORNER_ENTRY; CORNER_CACHE_SLOTS]);
}

/// The eight corner values of the cell containing `(x0, y0, z0)`, memoized.
///
/// Pure memoization of a pure function, so it cannot change a result — it is
/// here only because the corner subtree is the most expensive thing in the
/// graph.
fn corners_of(id: u32, inner: &Arc<Node>, x0: i32, y0: i32, z0: i32, w: i32, h: i32) -> [f64; 8] {
    if let Some(v) = grid_corners(id, inner, x0, y0, z0, w, h) {
        return v;
    }
    let slot = {
        let mut h = id as u64;
        h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (x0 as u32 as u64);
        h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y0 as u32 as u64);
        h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (z0 as u32 as u64);
        (h >> 48) as usize % CORNER_CACHE_SLOTS
    };

    if let Some(hit) = CORNER_CACHE.with(|c| {
        let e = c.borrow()[slot];
        if e.occupied && e.id == id && e.x0 == x0 && e.y0 == y0 && e.z0 == z0 {
            Some(e.corners)
        } else {
            None
        }
    }) {
        return hit;
    }

    let at =
        |dx: i32, dy: i32, dz: i32| inner.compute(Ctx::new(x0 + dx * w, y0 + dy * h, z0 + dz * w));
    let v = [
        at(0, 0, 0),
        at(0, 0, 1),
        at(0, 1, 0),
        at(0, 1, 1),
        at(1, 0, 0),
        at(1, 0, 1),
        at(1, 1, 0),
        at(1, 1, 1),
    ];
    CORNER_CACHE.with(|c| {
        c.borrow_mut()[slot] = CornerEntry {
            id,
            x0,
            y0,
            z0,
            corners: v,
            occupied: true,
        }
    });
    v
}

/// The corner lattice of one chunk, per interpolated node — vanilla's
/// `NoiseChunk` slices. Each corner is computed once and shared by the up to
/// eight cells that touch it; the per-cell cache above recomputes a shared
/// corner for every cell that misses, which made it five times the work.
struct ChunkGrid {
    x0: i32,
    z0: i32,
    min_y: i32,
    w: i32,
    h: i32,
    nx: usize,
    ny: usize,
    values: Vec<Vec<f64>>,
}

thread_local! {
    static CHUNK_GRID: std::cell::RefCell<Option<ChunkGrid>> = const { std::cell::RefCell::new(None) };
}

/// While alive, interpolated nodes on this thread share corners across the
/// chunk at `(chunk_x, chunk_z)`. Pure memoization: results are identical.
pub struct ChunkGridGuard(());

impl ChunkGridGuard {
    /// Activate the lattice for one chunk.
    pub fn enter(
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        height: i32,
        cell_width: i32,
        cell_height: i32,
    ) -> Self {
        CHUNK_GRID.with(|g| {
            *g.borrow_mut() = Some(ChunkGrid {
                x0: chunk_x * 16,
                z0: chunk_z * 16,
                min_y,
                w: cell_width,
                h: cell_height,
                nx: (16 / cell_width) as usize + 1,
                ny: (height / cell_height) as usize + 1,
                values: Vec::new(),
            })
        });
        ChunkGridGuard(())
    }
}

impl Drop for ChunkGridGuard {
    fn drop(&mut self) {
        CHUNK_GRID.with(|g| *g.borrow_mut() = None);
    }
}

fn grid_corners(
    id: u32,
    inner: &Arc<Node>,
    x0: i32,
    y0: i32,
    z0: i32,
    w: i32,
    h: i32,
) -> Option<[f64; 8]> {
    let (idx, nx, ny) = CHUNK_GRID.with(|g| {
        let g = g.borrow();
        let g = g.as_ref()?;
        if g.w != w || g.h != h {
            return None;
        }
        let (ix, iy, iz) = (x0 - g.x0, y0 - g.min_y, z0 - g.z0);
        if ix < 0 || iz < 0 || iy < 0 || ix % w != 0 || iz % w != 0 || iy % h != 0 {
            return None;
        }
        let (ix, iy, iz) = ((ix / w) as usize, (iy / h) as usize, (iz / w) as usize);
        if ix + 1 >= g.nx || iz + 1 >= g.nx || iy + 1 >= g.ny {
            return None;
        }
        Some(((ix, iy, iz), g.nx, g.ny))
    })?;
    let at = |dx: usize, dy: usize, dz: usize| ((idx.0 + dx) * ny + idx.1 + dy) * nx + idx.2 + dz;
    let order = [
        (0, 0, 0),
        (0, 0, 1),
        (0, 1, 0),
        (0, 1, 1),
        (1, 0, 0),
        (1, 0, 1),
        (1, 1, 0),
        (1, 1, 1),
    ];
    let mut out = [f64::NAN; 8];
    CHUNK_GRID.with(|g| {
        let mut g = g.borrow_mut();
        let Some(g) = g.as_mut() else { return };
        let size = g.nx * g.nx * g.ny;
        let id = id as usize;
        if g.values.len() <= id {
            g.values.resize(id + 1, Vec::new());
        }
        if g.values[id].is_empty() {
            g.values[id] = vec![f64::NAN; size];
        }
        let v = &g.values[id];
        for (k, (dx, dy, dz)) in order.iter().enumerate() {
            out[k] = v[at(*dx, *dy, *dz)];
        }
    });
    for (k, (dx, dy, dz)) in order.iter().enumerate() {
        if out[k].is_nan() {
            let c = inner.compute(Ctx::new(
                x0 + *dx as i32 * w,
                y0 + *dy as i32 * h,
                z0 + *dz as i32 * w,
            ));
            out[k] = c;
            let i = at(*dx, *dy, *dz);
            CHUNK_GRID.with(|g| {
                if let Some(g) = g.borrow_mut().as_mut() {
                    if let Some(v) = g.values.get_mut(id as usize) {
                        if !v.is_empty() {
                            v[i] = c;
                        }
                    }
                }
            });
        }
    }
    Some(out)
}

/// How many entries the marker cache remembers per thread, across every
/// `cache_once` / `cache_2d` node in the graph.
///
/// Several distinct cached subtrees are typically hot inside one chunk fill
/// (the aquifer's `erosion`, `depth`, `floodedness`, `spread`, `barrier`,
/// plus whatever `final_density` itself caches), so this is sized well past
/// the handful of ids the overworld graph actually assigns — collisions only
/// cost a recompute, never a wrong answer, but a table this size keeps them
/// rare.
const MARKER_CACHE_SLOTS: usize = 2048;

/// One marker-cache entry: the full key, not just a hash of it, so a slot
/// collision can only cause a miss — never hand back another position's
/// value.
#[derive(Clone, Copy)]
struct MarkerEntry {
    id: u32,
    x: i32,
    y: i32,
    z: i32,
    value: f64,
    occupied: bool,
}

const EMPTY_MARKER_ENTRY: MarkerEntry = MarkerEntry {
    id: u32::MAX,
    x: 0,
    y: 0,
    z: 0,
    value: 0.0,
    occupied: false,
};

thread_local! {
    static MARKER_CACHE: std::cell::RefCell<Vec<MarkerEntry>> =
        std::cell::RefCell::new(vec![EMPTY_MARKER_ENTRY; MARKER_CACHE_SLOTS]);
}

/// Memoize a pure function of `(id, x, y, z)`.
///
/// `id` distinguishes nodes; `(x, y, z)` is the exact key (callers key `y` to
/// `0` when a node is known not to depend on it, e.g. [`Node::Cache2D`]).
/// Because a slot collision is checked against the full key before use, this
/// can never return a different node's or a different position's value — the
/// worst a collision costs is a redundant recompute.
fn marker_cache(id: u32, x: i32, y: i32, z: i32, compute: impl FnOnce() -> f64) -> f64 {
    let slot = {
        // A simple multiplicative mix; only used to pick a slot, so it does
        // not need to be cryptographic, just well spread.
        let mut h = id as u64;
        h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (x as u32 as u64);
        h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u32 as u64);
        h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (z as u32 as u64);
        (h >> 48) as usize % MARKER_CACHE_SLOTS
    };

    if let Some(v) = MARKER_CACHE.with(|c| {
        let e = c.borrow()[slot];
        if e.occupied && e.id == id && e.x == x && e.y == y && e.z == z {
            Some(e.value)
        } else {
            None
        }
    }) {
        return v;
    }

    let value = compute();
    MARKER_CACHE.with(|c| {
        c.borrow_mut()[slot] = MarkerEntry {
            id,
            x,
            y,
            z,
            value,
            occupied: true,
        };
    });
    value
}

fn clamped_map(v: f64, from: f64, to: f64, from_value: f64, to_value: f64) -> f64 {
    let t = (v - from) / (to - from);
    if t < 0.0 {
        from_value
    } else if t > 1.0 {
        to_value
    } else {
        from_value + t * (to_value - from_value)
    }
}

// --- building --------------------------------------------------------------

/// A process-wide id for a caching node.
///
/// The memo tables are thread-local and shared by every graph on the thread —
/// the terrain's, the biome source's, a second world's — so the ids that key
/// them must be unique across graphs, not merely within one. Per-builder
/// counters made the terrain's cache #3 and the climate sampler's cache #3
/// the same entry, and a column whose biomes were sampled first then read the
/// climate's value back as its surface height.
fn next_cache_id() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Turns worldgen JSON into an evaluable [`Node`] graph.
///
/// Referenced functions are built once and shared, so a graph that names
/// `minecraft:overworld/continents` in six places evaluates one node six
/// times rather than six copies of it.
pub struct Builder<'a> {
    pack: &'a DataPack,
    noises: &'a NoiseRegistry,
    mode: Mode,
    memo: HashMap<String, Arc<Node>>,
    /// References currently being resolved, so a cyclic data pack is reported
    /// rather than overflowing the stack.
    in_flight: Vec<String>,
}

/// Which of vanilla's two readings of the same graph to build.
///
/// Vanilla keeps one set of JSON definitions and evaluates it two ways: the
/// plain `NoiseRouter`, where the cache and interpolation markers are
/// transparent, and the form a generating chunk uses, where they become real
/// caches and a real cell interpolation. The terrain a player walks on comes
/// from the second; the first is what the game itself exposes for point
/// queries. Building the wrong one silently gives numbers that look right and
/// are not the terrain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Markers are transparent. Matches the game's `NoiseRouter`.
    Router,
    /// `minecraft:interpolated` interpolates over a cell grid. Matches the
    /// game's `NoiseChunk`.
    Chunk {
        /// Cell size on X and Z, in blocks (`4 × size_horizontal`).
        cell_width: i32,
        /// Cell size on Y, in blocks (`4 × size_vertical`).
        cell_height: i32,
    },
}

impl<'a> Builder<'a> {
    /// A builder in [`Mode::Router`] — the form the game's own noise router
    /// evaluates.
    pub fn new(pack: &'a DataPack, noises: &'a NoiseRegistry) -> Self {
        Self::with_mode(pack, noises, Mode::Router)
    }

    /// A builder in an explicit [`Mode`].
    pub fn with_mode(pack: &'a DataPack, noises: &'a NoiseRegistry, mode: Mode) -> Self {
        Self {
            pack,
            noises,
            mode,
            memo: HashMap::new(),
            in_flight: Vec::new(),
        }
    }

    /// Build the density function named by `id`.
    pub fn reference(&mut self, id: &str) -> Result<Arc<Node>, BuildError> {
        let key = with_ns(id);
        if let Some(n) = self.memo.get(&key) {
            return Ok(Arc::clone(n));
        }
        if self.in_flight.contains(&key) {
            return err(format!(
                "cyclic density function reference: {} -> {key}",
                self.in_flight.join(" -> ")
            ));
        }
        let json = self.pack.density_function(&key)?;
        self.in_flight.push(key.clone());
        let built = self.build(&json);
        self.in_flight.pop();
        let node = built.map_err(|e| BuildError(format!("{key}: {e}")))?;
        self.memo.insert(key, Arc::clone(&node));
        Ok(node)
    }

    /// Build an inline density function value: a number, a reference string,
    /// or an object with a `type`.
    pub fn build(&mut self, v: &Json) -> Result<Arc<Node>, BuildError> {
        match v {
            Json::Num(n) => Ok(Arc::new(Node::Const(*n))),
            Json::Str(s) => self.reference(s),
            Json::Obj(_) => self.build_typed(v),
            other => err(format!("not a density function: {other:?}")),
        }
    }

    fn arg(&mut self, v: &Json, key: &str) -> Result<Arc<Node>, BuildError> {
        let inner = v
            .get(key)
            .ok_or_else(|| BuildError(format!("missing `{key}`")))?;
        self.build(inner)
    }

    fn num(v: &Json, key: &str) -> Result<f64, BuildError> {
        v.get(key)
            .and_then(Json::as_f64)
            .ok_or_else(|| BuildError(format!("missing number `{key}`")))
    }

    fn noise_of(&self, v: &Json, key: &str) -> Result<Arc<NormalNoise>, BuildError> {
        let id = v
            .get(key)
            .and_then(Json::as_str)
            .ok_or_else(|| BuildError(format!("missing noise id `{key}`")))?;
        self.noises.get(id)
    }

    fn build_typed(&mut self, v: &Json) -> Result<Arc<Node>, BuildError> {
        let ty = v
            .get("type")
            .and_then(Json::as_str)
            .ok_or_else(|| BuildError("object with no `type`".to_string()))?;
        let node = match strip_ns(ty) {
            "add" => Node::Add(self.arg(v, "argument1")?, self.arg(v, "argument2")?),
            "mul" => Node::Mul(self.arg(v, "argument1")?, self.arg(v, "argument2")?),
            "min" => Node::Min(self.arg(v, "argument1")?, self.arg(v, "argument2")?),
            "max" => Node::Max(self.arg(v, "argument1")?, self.arg(v, "argument2")?),
            "abs" => Node::Abs(self.arg(v, "argument")?),
            "square" => Node::Square(self.arg(v, "argument")?),
            "cube" => Node::Cube(self.arg(v, "argument")?),
            "half_negative" => Node::HalfNegative(self.arg(v, "argument")?),
            "quarter_negative" => Node::QuarterNegative(self.arg(v, "argument")?),
            "squeeze" => Node::Squeeze(self.arg(v, "argument")?),
            "invert" => Node::Invert(self.arg(v, "argument")?),
            // `cache_once` and `cache_all_in_cell` are both keyed on the exact
            // block position here (see `Node::CacheOnce`'s docs: correct for
            // both, just less reuse than vanilla's own `cache_all_in_cell`
            // array would give).
            "cache_once" | "cache_all_in_cell" => {
                let id = next_cache_id();
                Node::CacheOnce {
                    inner: self.arg(v, "argument")?,
                    id,
                }
            }
            // `cache_2d` and `flat_cache` both wrap functions vanilla itself
            // treats as column-only, so both are safe to key on `(x, z)`.
            "cache_2d" | "flat_cache" => {
                let id = next_cache_id();
                Node::Cache2D {
                    inner: self.arg(v, "argument")?,
                    id,
                }
            }
            "interpolated" => match self.mode {
                Mode::Router => Node::Interpolated(self.arg(v, "argument")?),
                Mode::Chunk {
                    cell_width,
                    cell_height,
                } => {
                    let id = next_cache_id();
                    Node::CellInterpolated {
                        inner: self.arg(v, "argument")?,
                        cell_width,
                        cell_height,
                        id,
                    }
                }
            },
            "blend_alpha" => Node::BlendAlpha,
            "blend_offset" => Node::BlendOffset,
            "blend_density" => Node::BlendDensity(self.arg(v, "argument")?),
            "noise" => Node::Noise {
                noise: self.noise_of(v, "noise")?,
                xz_scale: Self::num(v, "xz_scale")?,
                y_scale: Self::num(v, "y_scale")?,
            },
            "shifted_noise" => Node::ShiftedNoise {
                noise: self.noise_of(v, "noise")?,
                shift_x: self.arg(v, "shift_x")?,
                shift_y: self.arg(v, "shift_y")?,
                shift_z: self.arg(v, "shift_z")?,
                xz_scale: Self::num(v, "xz_scale")?,
                y_scale: Self::num(v, "y_scale")?,
            },
            // `argument` here names a *noise*, not a nested function — the one
            // place in the schema where that field changes meaning.
            "shift_a" => Node::ShiftA(self.noise_of(v, "argument")?),
            "shift_b" => Node::ShiftB(self.noise_of(v, "argument")?),
            "y_clamped_gradient" => Node::YClampedGradient {
                from_y: Self::num(v, "from_y")?,
                to_y: Self::num(v, "to_y")?,
                from_value: Self::num(v, "from_value")?,
                to_value: Self::num(v, "to_value")?,
            },
            "range_choice" => Node::RangeChoice {
                input: self.arg(v, "input")?,
                min_inclusive: Self::num(v, "min_inclusive")?,
                max_exclusive: Self::num(v, "max_exclusive")?,
                in_range: self.arg(v, "when_in_range")?,
                out_of_range: self.arg(v, "when_out_of_range")?,
            },
            "clamp" => Node::Clamp {
                input: self.arg(v, "input")?,
                min: Self::num(v, "min")?,
                max: Self::num(v, "max")?,
            },
            "interval_select" => {
                let thresholds: Vec<f64> = v
                    .get("thresholds")
                    .and_then(Json::as_arr)
                    .ok_or_else(|| BuildError("missing `thresholds`".into()))?
                    .iter()
                    .map(|t| t.as_f64().unwrap_or(f64::NAN))
                    .collect();
                let raw = v
                    .get("functions")
                    .and_then(Json::as_arr)
                    .ok_or_else(|| BuildError("missing `functions`".into()))?
                    .to_vec();
                if raw.len() != thresholds.len() + 1 {
                    return err(format!(
                        "interval_select: {} functions for {} thresholds",
                        raw.len(),
                        thresholds.len()
                    ));
                }
                let mut functions = Vec::with_capacity(raw.len());
                for f in &raw {
                    functions.push(self.build(f)?);
                }
                Node::IntervalSelect {
                    input: self.arg(v, "input")?,
                    thresholds,
                    functions,
                }
            }
            "find_top_surface" => Node::FindTopSurface {
                density: self.arg(v, "density")?,
                upper_bound: self.arg(v, "upper_bound")?,
                lower_bound: Self::num(v, "lower_bound")? as i32,
                cell_height: Self::num(v, "cell_height")? as i32,
            },
            "spline" => {
                let s = v
                    .get("spline")
                    .ok_or_else(|| BuildError("missing `spline`".into()))?;
                Node::SplineNode(self.build_spline(s)?)
            }
            // Unlike every other noise, this one is not named in the data:
            // the whole graph shares one stack, seeded from `minecraft:terrain`.
            "old_blended_noise" => Node::OldBlendedNoise(self.noises.blended_terrain(
                Self::num(v, "xz_scale")?,
                Self::num(v, "y_scale")?,
                Self::num(v, "xz_factor")?,
                Self::num(v, "y_factor")?,
                Self::num(v, "smear_scale_multiplier")?,
            )),
            "weird_scaled_sampler" => Node::WeirdScaledSampler {
                input: self.arg(v, "input")?,
                noise: self.noise_of(v, "noise")?,
                type_1: match v.get("rarity_value_mapper").and_then(Json::as_str) {
                    Some("type_1") => true,
                    Some("type_2") => false,
                    other => {
                        return err(format!(
                            "weird_scaled_sampler: bad rarity_value_mapper {other:?}"
                        ))
                    }
                },
            },
            "constant" => Node::Const(Self::num(v, "argument")?),
            "shift" => Node::Shift(self.noise_of(v, "argument")?),
            other => return err(format!("unsupported density function type `{other}`")),
        };
        Ok(Arc::new(node))
    }

    fn build_spline(&mut self, v: &Json) -> Result<Arc<Spline>, BuildError> {
        let coordinate = self.build(
            v.get("coordinate")
                .ok_or_else(|| BuildError("spline: missing `coordinate`".into()))?,
        )?;
        let points = v
            .get("points")
            .and_then(Json::as_arr)
            .ok_or_else(|| BuildError("spline: missing `points`".into()))?
            .to_vec();
        let mut locations = Vec::with_capacity(points.len());
        let mut values = Vec::with_capacity(points.len());
        let mut derivatives = Vec::with_capacity(points.len());
        for p in &points {
            locations.push(
                p.get("location")
                    .and_then(Json::as_f64)
                    .ok_or_else(|| BuildError("spline point: missing `location`".into()))?
                    as f32,
            );
            derivatives.push(
                p.get("derivative")
                    .and_then(Json::as_f64)
                    .ok_or_else(|| BuildError("spline point: missing `derivative`".into()))?
                    as f32,
            );
            let value = p
                .get("value")
                .ok_or_else(|| BuildError("spline point: missing `value`".into()))?;
            values.push(self.build_spline_value(value)?);
        }
        if locations.is_empty() {
            return err("spline: no points");
        }
        Ok(Arc::new(Spline {
            coordinate,
            locations,
            values,
            derivatives,
        }))
    }

    fn build_spline_value(&mut self, v: &Json) -> Result<SplineValue, BuildError> {
        match v {
            Json::Num(n) => Ok(SplineValue::Const(*n as f32)),
            Json::Obj(_) => Ok(SplineValue::Nested(self.build_spline(v)?)),
            other => err(format!(
                "spline value: expected number or spline, got {other:?}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(v: f64) -> Arc<Node> {
        Arc::new(Node::Const(v))
    }

    const C: Ctx = Ctx { x: 0, y: 0, z: 0 };

    #[test]
    fn arithmetic_operators_match_their_definitions() {
        assert_eq!(Node::Add(n(2.0), n(3.0)).compute(C), 5.0);
        assert_eq!(Node::Mul(n(2.0), n(3.0)).compute(C), 6.0);
        assert_eq!(Node::Min(n(2.0), n(3.0)).compute(C), 2.0);
        assert_eq!(Node::Max(n(2.0), n(3.0)).compute(C), 3.0);
        assert_eq!(Node::Abs(n(-2.0)).compute(C), 2.0);
        assert_eq!(Node::Square(n(-3.0)).compute(C), 9.0);
        assert_eq!(Node::Cube(n(-3.0)).compute(C), -27.0);
        assert_eq!(Node::HalfNegative(n(4.0)).compute(C), 4.0);
        assert_eq!(Node::HalfNegative(n(-4.0)).compute(C), -2.0);
        assert_eq!(Node::QuarterNegative(n(-4.0)).compute(C), -1.0);
        // squeeze clamps first, so anything past 1 gives the same answer.
        let at_one = 1.0 / 2.0 - 1.0 / 24.0;
        assert_eq!(Node::Squeeze(n(1.0)).compute(C), at_one);
        assert_eq!(Node::Squeeze(n(50.0)).compute(C), at_one);
    }

    #[test]
    fn y_clamped_gradient_ramps_then_holds() {
        let g = Node::YClampedGradient {
            from_y: -64.0,
            to_y: 320.0,
            from_value: 1.5,
            to_value: -1.5,
        };
        assert_eq!(g.compute(Ctx::new(0, -64, 0)), 1.5);
        assert_eq!(g.compute(Ctx::new(0, 320, 0)), -1.5);
        assert_eq!(g.compute(Ctx::new(0, -1000, 0)), 1.5);
        assert_eq!(g.compute(Ctx::new(0, 1000, 0)), -1.5);
        assert!((g.compute(Ctx::new(0, 128, 0)) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn range_choice_bounds_are_half_open() {
        let mk = |v: f64| Node::RangeChoice {
            input: n(v),
            min_inclusive: 0.0,
            max_exclusive: 1.0,
            in_range: n(10.0),
            out_of_range: n(-10.0),
        };
        assert_eq!(mk(0.0).compute(C), 10.0);
        assert_eq!(mk(0.999).compute(C), 10.0);
        assert_eq!(mk(1.0).compute(C), -10.0);
        assert_eq!(mk(-0.001).compute(C), -10.0);
    }

    #[test]
    fn caches_are_transparent() {
        // A marker must not change the value it wraps — including `flat_cache`,
        // whose quantizing form belongs to the chunk cache, not to the function.
        let inner = Arc::new(Node::YClampedGradient {
            from_y: 0.0,
            to_y: 100.0,
            from_value: 0.0,
            to_value: 100.0,
        });
        let m = Node::CacheOnce {
            inner: Arc::clone(&inner),
            id: 999,
        };
        for y in [-64, 0, 37, 300] {
            let c = Ctx::new(3, y, -7);
            assert_eq!(m.compute(c), inner.compute(c), "y={y}");
        }
    }
    #[test]
    fn a_flat_spline_is_its_constant_and_extends_by_its_slope() {
        let s = Spline {
            coordinate: Arc::new(Node::YClampedGradient {
                from_y: 0.0,
                to_y: 100.0,
                from_value: 0.0,
                to_value: 100.0,
            }),
            locations: vec![0.0, 10.0],
            values: vec![SplineValue::Const(1.0), SplineValue::Const(3.0)],
            derivatives: vec![0.0, 0.0],
        };
        assert_eq!(s.apply(Ctx::new(0, 0, 0)), 1.0);
        assert_eq!(s.apply(Ctx::new(0, 10, 0)), 3.0);
        // Zero end slopes hold flat outside the knots.
        assert_eq!(s.apply(Ctx::new(0, 50, 0)), 3.0);
        // Midpoint of a zero-slope pair is the plain average.
        assert_eq!(s.apply(Ctx::new(0, 5, 0)), 2.0);
    }

    #[test]
    fn an_unknown_operator_refuses_rather_than_guessing() {
        let pack = DataPack {
            worldgen: PathBuf::from("/nonexistent"),
        };
        let noises = NoiseRegistry::new(pack.clone(), 0);
        let mut b = Builder::new(&pack, &noises);
        let j = Json::parse(r#"{"type":"minecraft:not_a_real_operator"}"#).unwrap();
        let e = b.build(&j).unwrap_err().to_string();
        assert!(e.contains("unsupported density function type"), "{e}");
    }
}
