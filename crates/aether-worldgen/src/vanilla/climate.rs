//! The multi-noise climate sampler and the biome lookup it feeds.
//!
//! Six numbers decide the overworld's biomes — temperature, humidity,
//! continentalness, erosion, depth and weirdness — and a biome is whichever
//! entry of a big table of hyper-rectangles lies nearest to that six-vector.
//!
//! Two details decide whether this matches vanilla or merely resembles it:
//!
//! * **The sample is taken in `f32`.** The density graph computes in `f64`,
//!   but vanilla narrows each climate value to a float before quantizing it to
//!   a fixed-point `i64` at 1/10000. Keeping the extra precision would move
//!   points across biome boundaries.
//! * **Quantization is truncation, not rounding**, and it happens after the
//!   float multiply — `(long)(v * 10000.0f)`.
//!
//! # Nearest-entry search
//!
//! Vanilla stores the table in an R-tree and searches it with branch-and-bound
//! pruning, so the *distance* it finds is exactly the true minimum. Which entry
//! it returns when two are equidistant, though, depends on two things beyond
//! the distance: the order the bulk-loaded tree happens to put them in, and a
//! per-thread cache of the previous search's answer, which is seeded into the
//! bound and therefore wins any tie against it.
//!
//! Both are reproduced here — the tree is built by vanilla's own bulk-loading
//! procedure, and [`SearchCache`] is the cache. The cache is explicit rather
//! than ambient because it makes lookups depend on the order they were made in,
//! and a generator that quietly does that is a generator that is hard to trust:
//! [`ParameterList::find`] is cache-free and deterministic, and callers that
//! want to reproduce a specific vanilla call sequence opt in with
//! [`ParameterList::find_cached`].

use super::density::{BuildError, Ctx, Node};
use super::json::Json;
use std::sync::Arc;

/// Vanilla's fixed-point scale for climate coordinates.
const SCALE: f32 = 10_000.0;

/// `(long)(v * 10000.0f)` — the float multiply and the truncation both matter.
#[inline]
pub fn quantize(v: f32) -> i64 {
    (v * SCALE) as i64
}

/// One sampled climate vector, in vanilla's fixed-point units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetPoint {
    /// Temperature.
    pub temperature: i64,
    /// Humidity (the router calls this noise "vegetation").
    pub humidity: i64,
    /// Continentalness (the router calls this "continents").
    pub continentalness: i64,
    /// Erosion.
    pub erosion: i64,
    /// Depth below the terrain surface estimate.
    pub depth: i64,
    /// Weirdness (the router calls this "ridges").
    pub weirdness: i64,
}

impl TargetPoint {
    /// The six axes plus the constant zero the `offset` axis is measured
    /// against, in the order the search tree indexes them.
    fn to_axes(self) -> [i64; 7] {
        [
            self.temperature,
            self.humidity,
            self.continentalness,
            self.erosion,
            self.depth,
            self.weirdness,
            0,
        ]
    }

    fn axis(&self, i: usize) -> i64 {
        match i {
            0 => self.temperature,
            1 => self.humidity,
            2 => self.continentalness,
            3 => self.erosion,
            4 => self.depth,
            _ => self.weirdness,
        }
    }
}

/// An inclusive range on one climate axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    /// Lower bound.
    pub min: i64,
    /// Upper bound.
    pub max: i64,
}

impl Parameter {
    /// How far `value` lies outside this range; zero when inside.
    #[inline]
    fn distance(&self, value: i64) -> i64 {
        let above = value - self.max;
        if above > 0 {
            above
        } else {
            (self.min - value).max(0)
        }
    }
}

/// One row of the biome table: a box in climate space and the biome it names.
#[derive(Debug, Clone)]
pub struct ParameterPoint {
    /// The biome id, e.g. `minecraft:plains`.
    pub biome: String,
    /// The six axis ranges, in the order [`TargetPoint::axis`] uses.
    pub space: [Parameter; 6],
    /// A constant penalty added to every distance from this row.
    pub offset: i64,
}

impl ParameterPoint {
    /// The six ranges plus `offset` as a degenerate seventh, which is how the
    /// search tree sees a row.
    fn full_space(&self) -> [Parameter; 7] {
        let mut out = [Parameter { min: 0, max: 0 }; 7];
        out[..6].copy_from_slice(&self.space);
        out[6] = Parameter {
            min: self.offset,
            max: self.offset,
        };
        out
    }

    /// Squared distance from `target` to this box, vanilla's `fitness`.
    #[inline]
    pub fn fitness(&self, target: &TargetPoint) -> i64 {
        let mut sum = 0i64;
        for i in 0..6 {
            let d = self.space[i].distance(target.axis(i));
            sum += d * d;
        }
        sum + self.offset * self.offset
    }
}

/// How many axes a climate box has: the six sampled ones plus `offset`.
const AXES: usize = 7;

/// One node of the search tree: either a biome, or a box containing others.
#[derive(Debug, Clone)]
enum RNode {
    /// An index into [`ParameterList::entries`].
    Leaf(usize),
    /// A bounding box over its children.
    SubTree(Vec<Boxed>),
}

/// A node's bounding box plus the node itself. Kept side by side so the search
/// never has to walk into a subtree to know how far away it is.
#[derive(Debug, Clone)]
struct Boxed {
    space: [Parameter; AXES],
    node: RNode,
}

impl Boxed {
    /// Squared distance from `target` to this box.
    #[inline]
    fn distance(&self, target: &[i64; AXES]) -> i64 {
        let mut sum = 0i64;
        for i in 0..AXES {
            let d = self.space[i].distance(target[i]);
            sum += d * d;
        }
        sum
    }

    /// The key vanilla sorts by on one axis: the box's midpoint, optionally
    /// folded to its magnitude. Integer division truncates toward zero, which
    /// is what Java's `/` does and what the tree shape depends on.
    #[inline]
    fn midpoint(&self, axis: usize, abs: bool) -> i64 {
        let p = self.space[axis];
        let m = (p.min + p.max) / 2;
        if abs {
            m.abs()
        } else {
            m
        }
    }

    /// The total extent of the box, summed over every axis — vanilla's `cost`,
    /// the thing the bulk loader minimizes when it picks a split axis.
    fn cost(&self) -> i64 {
        self.space.iter().map(|p| (p.max - p.min).abs()).sum()
    }
}

/// Remembers the previous search, the way vanilla's per-thread cache does.
///
/// The cached leaf is used as the initial bound, so a *tie* with it is resolved
/// in its favour. Lookups made through a cache therefore depend on the order
/// they were made in; that is faithful to vanilla, and it is why the cache is
/// something a caller has to ask for.
#[derive(Debug, Clone, Default)]
pub struct SearchCache {
    last: Option<usize>,
}

impl SearchCache {
    /// A cache that has not seen a search yet.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The overworld biome table.
#[derive(Debug, Clone)]
pub struct ParameterList {
    entries: Vec<ParameterPoint>,
    root: Boxed,
}

impl ParameterList {
    /// Parse the table out of a `--reports` dump
    /// (`generated/reports/biome_parameters/minecraft/overworld.json`).
    ///
    /// The table is *code* in the game, not data — it exists on disk only in
    /// the report the server writes with `--reports`, which is why the caller
    /// has to point at one rather than at the data pack.
    pub fn from_report(json: &Json) -> Result<Self, BuildError> {
        let biomes = json
            .get("biomes")
            .and_then(Json::as_arr)
            .ok_or_else(|| BuildError::new("biome parameter report: no `biomes` array"))?;
        let mut entries = Vec::with_capacity(biomes.len());
        for b in biomes {
            let biome = b
                .get("biome")
                .and_then(Json::as_str)
                .ok_or_else(|| BuildError::new("biome entry: no `biome`"))?
                .to_string();
            let p = b
                .get("parameters")
                .ok_or_else(|| BuildError::new("biome entry: no `parameters`"))?;
            let axes = [
                "temperature",
                "humidity",
                "continentalness",
                "erosion",
                "depth",
                "weirdness",
            ];
            let mut space = [Parameter { min: 0, max: 0 }; 6];
            for (i, name) in axes.iter().enumerate() {
                space[i] = read_parameter(p, name)?;
            }
            let offset = read_parameter(p, "offset")?.min;
            entries.push(ParameterPoint {
                biome,
                space,
                offset,
            });
        }
        if entries.is_empty() {
            return Err(BuildError::new("biome parameter report is empty"));
        }
        Ok(Self::from_points(entries))
    }

    /// Build the table from rows, in the order given — the order is part of
    /// the search tree's shape (see the module docs). [`super::biome_table`]
    /// supplies the overworld's rows without needing a report.
    pub fn from_points(entries: Vec<ParameterPoint>) -> Self {
        assert!(!entries.is_empty(), "a biome table needs at least one row");
        let root = build_tree(
            entries
                .iter()
                .enumerate()
                .map(|(i, e)| Boxed {
                    space: e.full_space(),
                    node: RNode::Leaf(i),
                })
                .collect(),
        );
        Self { entries, root }
    }

    /// The rows, in insertion order.
    pub fn entries(&self) -> &[ParameterPoint] {
        &self.entries
    }

    /// How many rows the table has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The biome nearest `target`, with no search history.
    pub fn find(&self, target: &TargetPoint) -> &str {
        &self.entries[self.find_index(target, &mut SearchCache::new())].biome
    }

    /// The biome nearest `target`, remembering the answer in `cache` — vanilla's
    /// stateful search. Feed one cache through a sequence of lookups to
    /// reproduce that sequence exactly.
    pub fn find_cached(&self, target: &TargetPoint, cache: &mut SearchCache) -> &str {
        &self.entries[self.find_index(target, cache)].biome
    }

    /// The row index nearest `target`, with no search history.
    pub fn find_entry(&self, target: &TargetPoint) -> usize {
        self.search(&self.root, &target.to_axes(), None)
    }

    fn find_index(&self, target: &TargetPoint, cache: &mut SearchCache) -> usize {
        let t = target.to_axes();
        let found = self.search(&self.root, &t, cache.last);
        cache.last = Some(found);
        found
    }

    /// Vanilla's branch-and-bound descent.
    ///
    /// `best` is both the incumbent answer and the pruning bound: a subtree
    /// whose bounding box is no nearer than the incumbent is skipped outright,
    /// which is what makes this exact *and* fast — and also what makes a tie
    /// with the incumbent keep the incumbent.
    fn search(&self, node: &Boxed, target: &[i64; AXES], best: Option<usize>) -> usize {
        let children = match &node.node {
            RNode::Leaf(i) => return *i,
            RNode::SubTree(children) => children,
        };
        let mut bound = match best {
            Some(i) => self.leaf_distance(i, target),
            None => i64::MAX,
        };
        let mut leaf = best;
        for child in children {
            let m = child.distance(target);
            if bound > m {
                let candidate = self.search(child, target, leaf);
                // A leaf child *is* the candidate, so its box distance is
                // already the exact one — no need to recompute it.
                let n = match &child.node {
                    RNode::Leaf(i) if *i == candidate => m,
                    _ => self.leaf_distance(candidate, target),
                };
                if bound > n {
                    bound = n;
                    leaf = Some(candidate);
                }
            }
        }
        leaf.expect("a subtree always has at least one child")
    }

    #[inline]
    fn leaf_distance(&self, i: usize, target: &[i64; AXES]) -> i64 {
        let e = &self.entries[i];
        let mut sum = 0i64;
        for a in 0..6 {
            let d = e.space[a].distance(target[a]);
            sum += d * d;
        }
        sum + e.offset * e.offset
    }

    /// The biome nearest `target`, plus how many *distinct* biomes were tied
    /// at that distance.
    ///
    /// A count above one marks a position where the answer is decided by tree
    /// order and search history rather than by distance alone. Surfacing it
    /// keeps a legitimate ambiguity from being mistaken for a bug.
    pub fn find_with_ties(&self, target: &TargetPoint) -> (&str, usize) {
        let biome = self.find(target);
        let best = self
            .entries
            .iter()
            .map(|e| e.fitness(target))
            .min()
            .expect("non-empty table");
        let mut tied: Vec<&str> = Vec::new();
        for e in &self.entries {
            if e.fitness(target) == best && !tied.contains(&e.biome.as_str()) {
                tied.push(&e.biome);
            }
        }
        (biome, tied.len())
    }
}

/// Bulk-load the search tree exactly as vanilla does.
///
/// The shape of this tree is not an implementation detail: it decides which
/// of two equidistant biomes a lookup returns, so "a reasonable R-tree" is not
/// good enough — it has to be *this* R-tree.
fn build_tree(nodes: Vec<Boxed>) -> Boxed {
    build(nodes)
}

fn build(mut nodes: Vec<Boxed>) -> Boxed {
    assert!(!nodes.is_empty(), "need at least one child to build a node");
    if nodes.len() == 1 {
        return nodes.pop().expect("checked non-empty");
    }
    if nodes.len() <= CHILDREN_PER_NODE {
        // Small enough to be one node: order the children by how far their
        // boxes sit from the origin, summed over all axes.
        nodes.sort_by_key(|n| {
            (0..AXES)
                .map(|j| ((n.space[j].min + n.space[j].max) / 2).abs())
                .sum::<i64>()
        });
        return subtree(nodes);
    }

    // Try splitting along each axis in turn and keep the split whose buckets
    // have the least total extent — a cheap stand-in for "the tightest boxes".
    let mut best_cost = i64::MAX;
    let mut best_axis = 0usize;
    let mut best_buckets: Vec<Boxed> = Vec::new();
    for axis in 0..AXES {
        sort_by_axis(&mut nodes, axis, false);
        let buckets = bucketize(&nodes);
        let cost: i64 = buckets.iter().map(Boxed::cost).sum();
        if best_cost > cost {
            best_cost = cost;
            best_axis = axis;
            best_buckets = buckets;
        }
    }
    sort_by_axis(&mut best_buckets, best_axis, true);
    let children = best_buckets
        .into_iter()
        .map(|b| match b.node {
            RNode::SubTree(kids) => build(kids),
            leaf @ RNode::Leaf(_) => Boxed {
                space: b.space,
                node: leaf,
            },
        })
        .collect();
    subtree(children)
}

/// Vanilla's node fan-out.
const CHILDREN_PER_NODE: usize = 6;

/// Sort by one axis, then by every other axis in wrap-around order as
/// tie-breakers. The sort is stable, so equal keys keep their existing order —
/// which is part of the tree's shape.
fn sort_by_axis(nodes: &mut [Boxed], axis: usize, abs: bool) {
    nodes.sort_by(|a, b| {
        for k in 0..AXES {
            let j = (axis + k) % AXES;
            let ord = a.midpoint(j, abs).cmp(&b.midpoint(j, abs));
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
}

/// Group a sorted list into fixed-size runs.
///
/// The run length is the largest power of six that still leaves more than one
/// bucket, so the tree comes out roughly balanced at a fan-out of six.
fn bucketize(nodes: &[Boxed]) -> Vec<Boxed> {
    let per = 6f64.powf(((nodes.len() as f64 - 0.01).ln() / 6f64.ln()).floor()) as usize;
    let per = per.max(1);
    let mut out = Vec::new();
    let mut current: Vec<Boxed> = Vec::new();
    for n in nodes {
        current.push(n.clone());
        if current.len() >= per {
            out.push(subtree(std::mem::take(&mut current)));
        }
    }
    if !current.is_empty() {
        out.push(subtree(current));
    }
    out
}

/// Wrap children in a node whose box is their union.
fn subtree(children: Vec<Boxed>) -> Boxed {
    assert!(!children.is_empty(), "subtree needs at least one child");
    let mut space = children[0].space;
    for c in &children[1..] {
        for i in 0..AXES {
            space[i].min = space[i].min.min(c.space[i].min);
            space[i].max = space[i].max.max(c.space[i].max);
        }
    }
    Boxed {
        space,
        node: RNode::SubTree(children),
    }
}

fn read_parameter(p: &Json, name: &str) -> Result<Parameter, BuildError> {
    let v = p
        .get(name)
        .ok_or_else(|| BuildError::new(format!("biome parameters: no `{name}`")))?;
    match v {
        // A bare number is a degenerate range.
        Json::Num(n) => {
            let q = quantize(*n as f32);
            Ok(Parameter { min: q, max: q })
        }
        Json::Arr(a) if a.len() == 2 => {
            let lo = a[0]
                .as_f64()
                .ok_or_else(|| BuildError::new(format!("`{name}`: non-numeric bound")))?;
            let hi = a[1]
                .as_f64()
                .ok_or_else(|| BuildError::new(format!("`{name}`: non-numeric bound")))?;
            Ok(Parameter {
                min: quantize(lo as f32),
                max: quantize(hi as f32),
            })
        }
        other => Err(BuildError::new(format!("`{name}`: unexpected shape {other:?}"))),
    }
}

/// The six climate density functions, ready to sample.
#[derive(Debug, Clone)]
pub struct Sampler {
    /// `temperature` from the noise router.
    pub temperature: Arc<Node>,
    /// `vegetation` from the noise router.
    pub humidity: Arc<Node>,
    /// `continents` from the noise router.
    pub continentalness: Arc<Node>,
    /// `erosion` from the noise router.
    pub erosion: Arc<Node>,
    /// `depth` from the noise router.
    pub depth: Arc<Node>,
    /// `ridges` from the noise router.
    pub weirdness: Arc<Node>,
}

impl Sampler {
    /// Sample at a *quart* position — the 4×4×4 cell grid biomes are stored
    /// on. Vanilla converts to blocks by `quart << 2` and samples there.
    pub fn sample(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> TargetPoint {
        let ctx = Ctx::new(quart_x << 2, quart_y << 2, quart_z << 2);
        TargetPoint {
            temperature: quantize(self.temperature.compute(ctx) as f32),
            humidity: quantize(self.humidity.compute(ctx) as f32),
            continentalness: quantize(self.continentalness.compute(ctx) as f32),
            erosion: quantize(self.erosion.compute(ctx) as f32),
            depth: quantize(self.depth.compute(ctx) as f32),
            weirdness: quantize(self.weirdness.compute(ctx) as f32),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantization_truncates_toward_zero_in_float() {
        // Not round(): -0.00009 must land on 0, not on -1.
        assert_eq!(quantize(0.0), 0);
        assert_eq!(quantize(1.0), 10_000);
        assert_eq!(quantize(-1.0), -10_000);
        assert_eq!(quantize(0.00009), 0);
        assert_eq!(quantize(-0.00009), 0);
        assert_eq!(quantize(0.55), 5500);
        // The float multiply is part of the definition: doing it in f64 and
        // then truncating gives 449 here, the float path gives 450.
        let v = 0.045f32;
        assert_eq!(quantize(v), (v * 10_000.0f32) as i64);
    }

    #[test]
    fn distance_is_zero_inside_a_range_and_grows_outside() {
        let p = Parameter {
            min: -100,
            max: 100,
        };
        assert_eq!(p.distance(0), 0);
        assert_eq!(p.distance(-100), 0);
        assert_eq!(p.distance(100), 0);
        assert_eq!(p.distance(150), 50);
        assert_eq!(p.distance(-150), 50);
    }

    fn table() -> ParameterList {
        let json = Json::parse(
            r#"{"biomes":[
              {"biome":"minecraft:a","parameters":{"temperature":[-1.0,0.0],"humidity":[-1.0,1.0],
                "continentalness":[-1.0,1.0],"erosion":[-1.0,1.0],"depth":0.0,"weirdness":[-1.0,1.0],"offset":0.0}},
              {"biome":"minecraft:b","parameters":{"temperature":[0.5,1.0],"humidity":[-1.0,1.0],
                "continentalness":[-1.0,1.0],"erosion":[-1.0,1.0],"depth":0.0,"weirdness":[-1.0,1.0],"offset":0.0}}
            ]}"#,
        )
        .unwrap();
        ParameterList::from_report(&json).unwrap()
    }

    fn target(temperature: f32) -> TargetPoint {
        TargetPoint {
            temperature: quantize(temperature),
            humidity: 0,
            continentalness: 0,
            erosion: 0,
            depth: 0,
            weirdness: 0,
        }
    }

    #[test]
    fn the_nearest_box_wins_and_containment_beats_proximity() {
        let t = table();
        assert_eq!(t.len(), 2);
        assert_eq!(t.find(&target(-0.5)), "minecraft:a");
        assert_eq!(t.find(&target(0.75)), "minecraft:b");
        // Outside both: nearest edge decides.
        assert_eq!(t.find(&target(0.2)), "minecraft:a");
        assert_eq!(t.find(&target(0.4)), "minecraft:b");
    }

    #[test]
    fn exact_ties_are_reported_rather_than_hidden() {
        let t = table();
        // Equidistant from a's max (0.0) and b's min (0.5).
        let (_, ties) = t.find_with_ties(&target(0.25));
        assert_eq!(ties, 2);
        let (_, ties) = t.find_with_ties(&target(-0.5));
        assert_eq!(ties, 1);
    }

    #[test]
    fn a_bare_number_parameter_is_a_point_range() {
        let t = table();
        assert_eq!(t.entries[0].space[4], Parameter { min: 0, max: 0 });
    }
}
