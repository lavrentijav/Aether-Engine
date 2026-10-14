//! Structure templates: the `.nbt` files under `data/minecraft/structure/`.
//!
//! A template is a palette plus a list of `(pos, state)` blocks; placing it
//! transforms each block by a mirror, a rotation about a rotation pivot, and a
//! world position, may run the pack's structure processors over it, then writes
//! the rotated block state. This is the single port of `StructureTemplate` and
//! `StructureTemplate.placeInWorld` used by both the igloo ([`super::igloo`])
//! and the fossil feature ([`crate::vanilla::feature::features`]):
//!
//! * [`StructureTemplate::from_gzip`] parses the (gzipped) NBT: `size`,
//!   `palette`, `blocks` (and the first of `palettes`, when a template has
//!   several);
//! * [`transform`] is `StructureTemplate.transform`;
//! * [`transform_state`] is `BlockState.mirror(mirror).rotate(rotation)` for the
//!   property shapes the overworld structures use (directional `facing`, the
//!   `axis` of a pillar, the four cross-connection booleans, a 16-way
//!   `rotation`); anything else is left as it was;
//! * [`StructureTemplate::place_in_world`] is `placeInWorld`, driven by a
//!   [`PlaceSettings`]: the processors (`block_rot`, `rule`, `protected_blocks`)
//!   are run in the pack's order and their random draws are part of the
//!   caller's seed, so that order is preserved exactly;
//! * [`load_from_pack`] reads a template the way the game builds its palette
//!   (`buildInfoList`: sorted by Y, then X, then Z), which the fossil's
//!   per-block processor draws depend on.
//!
//! `structure_block` handling is the caller's choice: with
//! [`PlaceSettings::ignore_structure_blocks`] a `structure_block` marker is
//! dropped (`BlockIgnoreProcessor.STRUCTURE_BLOCK`) and a `data` marker leaves
//! air behind, as `TemplateStructurePiece`'s `handleDataMarker` does for the
//! igloo's chest marker.
//!
//! `TemplateManager` reads and caches templates from the operator's own data
//! pack, never vendoring them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

use crate::vanilla::blockinfo;
use crate::vanilla::density::{BuildError, DataPack};
use crate::vanilla::feature::blocks::RuleTest;
use crate::vanilla::feature::level::{Level, Pos};
use crate::vanilla::json::Json;
use crate::vanilla::mth;
use crate::vanilla::rng::{Rng, WorldgenRandom};
use crate::vanilla::surface::parse_block_state as parse_json_block_state;
use crate::vanilla::tags::{BlockSet, Tags};

/// `Mirror`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirror {
    /// `none`.
    None,
    /// `left_right` — inverts Z.
    LeftRight,
    /// `front_back` — inverts X.
    FrontBack,
}

/// `Rotation` (about the Y axis), in the game's declaration order so
/// `Rotation.getRandom` indexes it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// `none`.
    None,
    /// `clockwise_90`.
    Clockwise90,
    /// `180`.
    Clockwise180,
    /// `counterclockwise_90`.
    Counterclockwise90,
}

impl Rotation {
    /// `Rotation.values()` — the order `getRandom` draws from.
    pub const ALL: [Rotation; 4] = [
        Rotation::None,
        Rotation::Clockwise90,
        Rotation::Clockwise180,
        Rotation::Counterclockwise90,
    ];

    /// `Rotation.getRandom`.
    pub fn random(r: &mut impl Rng) -> Self {
        Self::ALL[r.next_int_bounded(4) as usize]
    }

    /// `Rotation.rotate(int, int)` for a `count`-way rotation index.
    pub fn rotate_index(self, i: i32, count: i32) -> i32 {
        match self {
            Rotation::Clockwise90 => (i + count / 4) % count,
            Rotation::Clockwise180 => (i + count / 2) % count,
            Rotation::Counterclockwise90 => (i + count * 3 / 4) % count,
            Rotation::None => i,
        }
    }
}

/// One block of a template.
#[derive(Debug, Clone, Copy)]
pub struct TemplateBlock {
    /// Position within the template.
    pub pos: [i32; 3],
    /// The block state.
    pub state: BlockStateId,
}

/// A parsed structure template.
#[derive(Debug, Clone)]
pub struct StructureTemplate {
    /// `size`.
    pub size: [i32; 3],
    /// Every block, in the order the file lists them.
    pub blocks: Vec<TemplateBlock>,
}

impl StructureTemplate {
    /// Parse a gzipped `.nbt` buffer.
    pub fn from_gzip(data: &[u8]) -> Result<Self, String> {
        let raw = gunzip(data)?;
        Self::from_bytes(&raw)
    }

    /// Parse an uncompressed NBT buffer.
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        let root = Nbt::parse(data)?;
        let size = int_list(root.get("size")).unwrap_or([0, 0, 0]);

        // A single `palette`, or the first of `palettes`.
        let palette_json = root
            .get("palettes")
            .and_then(Nbt::as_list)
            .and_then(|l| l.first())
            .or_else(|| root.get("palette"))
            .ok_or("template has no palette")?;
        let mut palette: Vec<BlockStateId> = Vec::new();
        for p in palette_json.as_list().unwrap_or(&[]) {
            palette.push(parse_palette_state(p)?);
        }

        let mut blocks = Vec::new();
        if let Some(list) = root.get("blocks").and_then(Nbt::as_list) {
            for b in list {
                let pos = int_list(b.get("pos")).ok_or("block without pos")?;
                let idx = b.get("state").and_then(Nbt::as_i64).unwrap_or(0) as usize;
                let state = *palette.get(idx).unwrap_or(&BlockStateId::AIR);
                blocks.push(TemplateBlock { pos, state });
            }
        }
        Ok(Self { size, blocks })
    }

    /// `StructureTemplate.placeInWorld`: process and place every block into
    /// `lv`, `origin` the placement position. See [`PlaceSettings`].
    pub fn place_in_world(
        &self,
        lv: &mut Level,
        origin: Pos,
        s: &PlaceSettings,
        r: &mut WorldgenRandom,
    ) {
        // `StructurePlaceSettings.getRandomPalette`: a single-palette template
        // still draws `nextInt(1)` when the settings carry a random, so the
        // caller's stream advances here even though the pick is always palette
        // 0. The igloo carries no random, so it draws nothing.
        if s.random_palette {
            let _ = r.next_int_bounded(1);
        }
        // Processors read the world before any of this template's writes, so
        // the whole list is built first and only then written, as
        // `StructureTemplate.placeInWorld` does.
        let mut processed: Vec<(Pos, BlockStateId)> = Vec::with_capacity(self.blocks.len());
        for b in &self.blocks {
            let rel = transform(b.pos, s.mirror, s.rotation, s.pivot);
            let pos = Pos::new(origin.x + rel[0], origin.y + rel[1], origin.z + rel[2]);
            if s.ignore_structure_blocks && blockinfo::name(b.state) == "minecraft:structure_block"
            {
                if is_data_marker(b.state) {
                    processed.push((pos, BlockStateId::AIR));
                }
                continue;
            }
            let mut state = Some(b.state);
            for p in s.processors {
                match state.and_then(|st| p.process(&*lv, pos, st, r)) {
                    Some(st) => state = Some(st),
                    None => {
                        state = None;
                        break;
                    }
                }
            }
            if let Some(st) = state {
                processed.push((pos, transform_state(st, s.mirror, s.rotation)));
            }
        }
        for (pos, st) in processed {
            if let Some((min, max)) = s.bounding_box {
                if pos.x < min.x
                    || pos.x > max.x
                    || pos.y < min.y
                    || pos.y > max.y
                    || pos.z < min.z
                    || pos.z > max.z
                {
                    continue;
                }
            }
            lv.set(pos, st);
        }
    }
}

/// `StructurePlaceSettings` — how a template is placed.
#[derive(Debug, Clone)]
pub struct PlaceSettings<'a> {
    /// The rotation about the pivot.
    pub rotation: Rotation,
    /// The mirror, applied before the rotation.
    pub mirror: Mirror,
    /// The rotation pivot (`StructurePlaceSettings.setRotationPivot`).
    pub pivot: [i32; 3],
    /// The processors to run, in order.
    pub processors: &'a [Processor],
    /// Writes are dropped outside this box, when set.
    pub bounding_box: Option<(Pos, Pos)>,
    /// Draw the palette index from the random (`setRandom`); the igloo, which
    /// does not set one, leaves this false.
    pub random_palette: bool,
    /// Drop `structure_block`s, a `data` marker becoming air.
    pub ignore_structure_blocks: bool,
}

impl Default for PlaceSettings<'_> {
    fn default() -> Self {
        Self {
            rotation: Rotation::None,
            mirror: Mirror::None,
            pivot: [0, 0, 0],
            processors: &[],
            bounding_box: None,
            random_palette: false,
            ignore_structure_blocks: false,
        }
    }
}

/// Whether a `structure_block` is a `data` marker (`mode=data`), the kind
/// `TemplateStructurePiece` routes to `handleDataMarker`.
fn is_data_marker(state: BlockStateId) -> bool {
    let block = blockinfo::block_of(state);
    let (lo, _) = blocks::STATE_RANGE[block as usize];
    props::values_of(block, state.0 - lo)
        .iter()
        .any(|(n, v)| *n == "mode" && *v == "data")
}

/// `StructureTemplate.transform(pos, mirror, rotation, pivot)`.
pub fn transform(pos: [i32; 3], mirror: Mirror, rotation: Rotation, pivot: [i32; 3]) -> [i32; 3] {
    let [mut i, j, mut k] = pos;
    let mut mirrored = true;
    match mirror {
        Mirror::LeftRight => k = -k,
        Mirror::FrontBack => i = -i,
        Mirror::None => mirrored = false,
    }
    let l = pivot[0];
    let m = pivot[2];
    match rotation {
        Rotation::Counterclockwise90 => [l - m + k, j, l + m - i],
        Rotation::Clockwise90 => [l + m - k, j, m - l + i],
        Rotation::Clockwise180 => [l + l - i, j, m + m - k],
        Rotation::None => {
            if mirrored {
                [i, j, k]
            } else {
                pos
            }
        }
    }
}

/// `StructureTemplate.getZeroPositionWithTransform(pos, mirror, rotation)` for
/// a template of `size`, mirror `none`.
pub fn zero_position(base: Pos, rotation: Rotation, size: [i32; 3]) -> Pos {
    let (i, j) = (size[0] - 1, size[2] - 1);
    match rotation {
        Rotation::None => base,
        Rotation::Clockwise90 => base.offset(j, 0, 0),
        Rotation::Clockwise180 => base.offset(i, 0, j),
        Rotation::Counterclockwise90 => base.offset(0, 0, i),
    }
}

/// `StructureTemplate.getBoundingBox(pos, rotation, mirror)` for a template of
/// `size`, mirror `none`, as `(min, max)`.
pub fn bounding_box(base: Pos, rotation: Rotation, size: [i32; 3]) -> (Pos, Pos) {
    let a = [0, 0, 0];
    let b = transform(
        [size[0] - 1, size[1] - 1, size[2] - 1],
        Mirror::None,
        rotation,
        [0, 0, 0],
    );
    let min = Pos::new(a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2]));
    let max = Pos::new(a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2]));
    (
        min.offset(base.x, base.y, base.z),
        max.offset(base.x, base.y, base.z),
    )
}

/// `BlockState.mirror(mirror).rotate(rotation)`, for the property shapes the
/// overworld structures use.
pub fn transform_state(state: BlockStateId, mirror: Mirror, rotation: Rotation) -> BlockStateId {
    let s = mirror_state(state, mirror);
    rotate_state(s, rotation)
}

fn rotate_state(state: BlockStateId, rotation: Rotation) -> BlockStateId {
    if rotation == Rotation::None {
        return state;
    }
    let block = blockinfo::block_of(state);
    let (lo, _) = blocks::STATE_RANGE[block as usize];
    let vals = props::values_of(block, state.0 - lo);
    let sides = side_values(&vals);
    let mut out: Vec<(&str, String)> = Vec::with_capacity(vals.len());
    for (name, value) in &vals {
        let nv = match *name {
            "facing" => rotate_dir(value, rotation).map(str::to_string),
            "axis" => rotate_axis(value, rotation).map(str::to_string),
            "north" | "south" | "east" | "west" => {
                // new[side] = old[rotation⁻¹(side)] — the connection that was
                // at `side` moves to `rotate(side)`.
                let inv = inverse_dir(name, rotation);
                sides.get(inv).map(|v| (*v).to_string())
            }
            "rotation" => value
                .parse::<i32>()
                .ok()
                .map(|v| rotation.rotate_index(v, 16).to_string()),
            _ => None,
        };
        out.push((name, nv.unwrap_or_else(|| (*value).to_string())));
    }
    let refs: Vec<(&str, &str)> = out.iter().map(|(k, v)| (*k, v.as_str())).collect();
    props::state_with(block, &refs)
        .map(BlockStateId)
        .unwrap_or(state)
}

fn mirror_state(state: BlockStateId, mirror: Mirror) -> BlockStateId {
    if mirror == Mirror::None {
        return state;
    }
    let block = blockinfo::block_of(state);
    let (lo, _) = blocks::STATE_RANGE[block as usize];
    let vals = props::values_of(block, state.0 - lo);
    let sides = side_values(&vals);
    let mut out: Vec<(&str, String)> = Vec::with_capacity(vals.len());
    for (name, value) in &vals {
        let nv = match *name {
            "facing" => mirror_dir(value, mirror).map(str::to_string),
            "north" | "south" | "east" | "west" => {
                let inv = mirror_dir(name, mirror).unwrap_or(name);
                sides.get(inv).map(|v| (*v).to_string())
            }
            _ => None,
        };
        out.push((name, nv.unwrap_or_else(|| (*value).to_string())));
    }
    let refs: Vec<(&str, &str)> = out.iter().map(|(k, v)| (*k, v.as_str())).collect();
    props::state_with(block, &refs)
        .map(BlockStateId)
        .unwrap_or(state)
}

fn side_values<'a>(vals: &[(&'a str, &'a str)]) -> HashMap<&'a str, &'a str> {
    vals.iter()
        .filter(|(n, _)| matches!(*n, "north" | "south" | "east" | "west"))
        .map(|(n, v)| (*n, *v))
        .collect()
}

fn rotate_dir(d: &str, rotation: Rotation) -> Option<&'static str> {
    let cw = |d: &str| -> Option<&'static str> {
        match d {
            "north" => Some("east"),
            "east" => Some("south"),
            "south" => Some("west"),
            "west" => Some("north"),
            _ => None,
        }
    };
    match rotation {
        Rotation::None => None,
        Rotation::Clockwise90 => cw(d),
        Rotation::Clockwise180 => match d {
            "north" => Some("south"),
            "south" => Some("north"),
            "east" => Some("west"),
            "west" => Some("east"),
            _ => None,
        },
        Rotation::Counterclockwise90 => match d {
            "north" => Some("west"),
            "west" => Some("south"),
            "south" => Some("east"),
            "east" => Some("north"),
            _ => None,
        },
    }
}

/// The direction that `rotation` maps onto `d`.
fn inverse_dir(d: &str, rotation: Rotation) -> &'static str {
    let base = match d {
        "north" => "north",
        "south" => "south",
        "east" => "east",
        "west" => "west",
        _ => return "north",
    };
    match rotation {
        Rotation::Clockwise90 => match base {
            "north" => "west",
            "east" => "north",
            "south" => "east",
            _ => "south",
        },
        Rotation::Clockwise180 => match base {
            "north" => "south",
            "south" => "north",
            "east" => "west",
            _ => "east",
        },
        Rotation::Counterclockwise90 => match base {
            "north" => "east",
            "east" => "south",
            "south" => "west",
            _ => "north",
        },
        Rotation::None => base,
    }
}

fn mirror_dir(d: &str, mirror: Mirror) -> Option<&'static str> {
    match (mirror, d) {
        (Mirror::FrontBack, "east") => Some("west"),
        (Mirror::FrontBack, "west") => Some("east"),
        (Mirror::LeftRight, "north") => Some("south"),
        (Mirror::LeftRight, "south") => Some("north"),
        _ => None,
    }
}

fn rotate_axis(a: &str, rotation: Rotation) -> Option<&'static str> {
    match rotation {
        Rotation::Clockwise90 | Rotation::Counterclockwise90 => match a {
            "x" => Some("z"),
            "z" => Some("x"),
            _ => None,
        },
        _ => None,
    }
}

// --- processors ------------------------------------------------------------

/// A location predicate for `rule` processors: `always_true` /
/// `block_match` / `tag_match`, tested against the world block.
#[derive(Debug)]
pub enum RuleLocation {
    /// `always_true`.
    Always,
    /// `block_match`.
    Block(u16),
    /// `tag_match`.
    Tag(BlockSet),
}

impl RuleLocation {
    fn test(&self, lv: &Level, p: Pos) -> bool {
        match self {
            RuleLocation::Always => true,
            RuleLocation::Block(b) => blockinfo::block_of(lv.get(p)) == *b,
            RuleLocation::Tag(t) => t.contains(lv.get(p)),
        }
    }
}

/// One `ProcessorRule`.
#[derive(Debug)]
pub struct Rule {
    input: RuleTest,
    location: RuleLocation,
    output: BlockStateId,
}

/// A structure processor.
#[derive(Debug)]
pub enum Processor {
    /// `block_rot`.
    BlockRot {
        /// The integrity (chance a block survives).
        integrity: f32,
        /// The blocks the rot applies to, or all when `None`.
        rottable: Option<BlockSet>,
    },
    /// `rule`.
    Rule(Vec<Rule>),
    /// `protected_blocks`.
    Protected {
        /// The blocks that survive (are not dropped).
        cannot: BlockSet,
    },
}

impl Processor {
    /// Run one processor over a block. `None` means the block is dropped.
    fn process(
        &self,
        lv: &Level,
        pos: Pos,
        state: BlockStateId,
        r: &mut WorldgenRandom,
    ) -> Option<BlockStateId> {
        match self {
            Processor::BlockRot {
                integrity,
                rottable,
            } => {
                let applies = rottable.as_ref().map(|s| s.contains(state)).unwrap_or(true);
                if applies && r.next_float() > *integrity {
                    None
                } else {
                    Some(state)
                }
            }
            Processor::Rule(rules) => {
                // `RandomSource.create(Mth.getSeed(pos))`, fresh per block.
                let mut pos_r = WorldgenRandom::legacy(mth::get_seed(pos.x, pos.y, pos.z));
                for rule in rules {
                    if rule.input.test(state, &mut pos_r) && rule.location.test(lv, pos) {
                        return Some(rule.output);
                    }
                }
                Some(state)
            }
            Processor::Protected { cannot } => {
                if cannot.contains(lv.get(pos)) {
                    None
                } else {
                    Some(state)
                }
            }
        }
    }
}

/// Parse a `processor_list` JSON file.
pub fn parse_processors(j: &Json, tags: &Tags) -> Result<Vec<Processor>, BuildError> {
    let mut out = Vec::new();
    for p in j.get("processors").and_then(Json::as_arr).unwrap_or(&[]) {
        let ty = p
            .str_of("processor_type")
            .unwrap_or("")
            .trim_start_matches("minecraft:");
        out.push(match ty {
            "block_rot" => Processor::BlockRot {
                integrity: p.f64_or("integrity", 1.0) as f32,
                rottable: p.get("rottable_blocks").map(|v| tags.holder_set(v)),
            },
            "rule" => {
                let mut rules = Vec::new();
                for r in p.get("rules").and_then(Json::as_arr).unwrap_or(&[]) {
                    let input = RuleTest::parse(
                        r.get("input_predicate")
                            .ok_or_else(|| BuildError::new("rule: no input_predicate"))?,
                        tags,
                    )?;
                    let loc = r
                        .get("location_predicate")
                        .ok_or_else(|| BuildError::new("rule: no location_predicate"))?;
                    let location = match loc
                        .str_of("predicate_type")
                        .unwrap_or("")
                        .trim_start_matches("minecraft:")
                    {
                        "always_true" => RuleLocation::Always,
                        "block_match" => RuleLocation::Block(
                            loc.str_of("block")
                                .and_then(aether_world::registry::blocks::block_id_of)
                                .ok_or_else(|| BuildError::new("rule: bad location block"))?,
                        ),
                        "tag_match" => {
                            RuleLocation::Tag(tags.block_tag(loc.str_of("tag").unwrap_or("")))
                        }
                        other => {
                            return Err(BuildError::new(format!(
                                "unknown location predicate `{other}`"
                            )))
                        }
                    };
                    let output = r
                        .get("output_state")
                        .and_then(parse_json_block_state)
                        .ok_or_else(|| BuildError::new("rule: bad output_state"))?;
                    rules.push(Rule {
                        input,
                        location,
                        output,
                    });
                }
                Processor::Rule(rules)
            }
            "protected_blocks" => Processor::Protected {
                cannot: tags.holder_set(p.get("value").unwrap_or(&Json::Null)),
            },
            other => {
                return Err(BuildError::new(format!(
                    "unknown structure processor `{other}`"
                )))
            }
        });
    }
    Ok(out)
}

/// Read a structure template from the pack, with its blocks in the game's
/// placement order (`buildInfoList`: sorted by Y, then X, then Z).
///
/// That order is what `StructureTemplate.placeInWorld` iterates, so it is also
/// the order per-block processors draw from the caller's random.
pub fn load_from_pack(pack: &DataPack, id: &str) -> Result<StructureTemplate, BuildError> {
    let bytes = pack.read_structure(id)?;
    let mut t = StructureTemplate::from_gzip(&bytes)
        .map_err(|e| BuildError::new(format!("cannot parse structure `{id}`: {e}")))?;
    t.blocks.sort_by_key(|b| (b.pos[1], b.pos[0], b.pos[2]));
    Ok(t)
}

fn parse_palette_state(j: &Nbt) -> Result<BlockStateId, String> {
    let name = j
        .get("Name")
        .and_then(Nbt::as_str)
        .ok_or("palette entry without Name")?;
    let block = blocks::block_id_of(name).ok_or_else(|| format!("unknown block `{name}`"))?;
    let props: Vec<(String, String)> = j
        .get("Properties")
        .and_then(Nbt::as_compound)
        .map(|c| {
            c.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let refs: Vec<(&str, &str)> = props
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    props::state_with(block, &refs)
        .map(BlockStateId)
        .ok_or_else(|| format!("cannot build state for `{name}`"))
}

fn int_list(j: Option<&Nbt>) -> Option<[i32; 3]> {
    let j = j?;
    let v: Vec<i32> = match j {
        Nbt::IntArray(a) => a.clone(),
        Nbt::List(items) => items
            .iter()
            .map(|n| n.as_i64().map(|v| v as i32))
            .collect::<Option<Vec<i32>>>()?,
        _ => return None,
    };
    (v.len() == 3).then(|| [v[0], v[1], v[2]])
}

/// Reads and caches templates from a data pack's `data/minecraft/structure/`.
pub struct TemplateManager {
    dir: PathBuf,
    cache: Mutex<HashMap<String, Arc<StructureTemplate>>>,
}

impl TemplateManager {
    /// `pack_root` is the directory that holds `data/` (the same root
    /// [`crate::vanilla::density::DataPack`] takes).
    pub fn new(pack_root: impl AsRef<Path>) -> Self {
        Self {
            dir: pack_root.as_ref().join("data/minecraft/structure"),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Load `id` (e.g. `minecraft:igloo/top`), or `None` when absent or broken.
    pub fn get(&self, id: &str) -> Option<Arc<StructureTemplate>> {
        let rel = id.strip_prefix("minecraft:").unwrap_or(id);
        if let Some(t) = self.cache.lock().unwrap().get(rel) {
            return Some(Arc::clone(t));
        }
        let path = self.dir.join(format!("{rel}.nbt"));
        let data = std::fs::read(&path).ok()?;
        let t = Arc::new(StructureTemplate::from_gzip(&data).ok()?);
        self.cache
            .lock()
            .unwrap()
            .insert(rel.to_string(), Arc::clone(&t));
        Some(t)
    }
}

/// Inflate a gzip stream: parse the header, raw-inflate the deflate body.
fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let flg = data[3];
    let mut i = 10usize;
    if flg & 0x04 != 0 {
        // FEXTRA
        if i + 2 > data.len() {
            return Err("truncated gzip header".into());
        }
        let xlen = u16::from_le_bytes([data[i], data[i + 1]]) as usize;
        i += 2 + xlen;
    }
    if flg & 0x08 != 0 {
        // FNAME
        i = skip_cstr(data, i)?;
    }
    if flg & 0x10 != 0 {
        // FCOMMENT
        i = skip_cstr(data, i)?;
    }
    if flg & 0x02 != 0 {
        // FHCRC
        i += 2;
    }
    let end = data.len().checked_sub(8).ok_or("truncated gzip trailer")?;
    if i > end {
        return Err("truncated gzip body".into());
    }
    miniz_oxide::inflate::decompress_to_vec(&data[i..end])
        .map_err(|e| format!("gzip inflate: {e:?}"))
}

fn skip_cstr(data: &[u8], mut i: usize) -> Result<usize, String> {
    while i < data.len() && data[i] != 0 {
        i += 1;
    }
    if i >= data.len() {
        return Err("truncated gzip string".into());
    }
    Ok(i + 1)
}

// --- minimal NBT ----------------------------------------------------------

/// A parsed NBT value (owned, read-only).
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Nbt>),
    Compound(Vec<(String, Nbt)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Nbt {
    fn parse(buf: &[u8]) -> Result<Self, String> {
        let mut c = Cursor {
            buf,
            pos: 0,
            depth: 0,
        };
        let tag = c.u8()?;
        if tag != 10 {
            return Err("NBT root is not a compound".into());
        }
        let _name = c.string()?;
        c.payload(10)
    }

    fn get(&self, key: &str) -> Option<&Nbt> {
        match self {
            Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_compound(&self) -> Option<&[(String, Nbt)]> {
        match self {
            Nbt::Compound(fields) => Some(fields),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Nbt::String(s) => Some(s),
            _ => None,
        }
    }
    fn as_i64(&self) -> Option<i64> {
        match self {
            Nbt::Byte(v) => Some(*v as i64),
            Nbt::Short(v) => Some(*v as i64),
            Nbt::Int(v) => Some(*v as i64),
            Nbt::Long(v) => Some(*v),
            _ => None,
        }
    }
    fn as_list(&self) -> Option<&[Nbt]> {
        match self {
            Nbt::List(items) => Some(items),
            _ => None,
        }
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    depth: u32,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.pos.checked_add(n).ok_or("NBT truncated")?;
        let s = self.buf.get(self.pos..end).ok_or("NBT truncated")?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, String> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(|s| s.to_owned())
            .map_err(|_| "invalid NBT string".into())
    }

    fn payload(&mut self, tag: u8) -> Result<Nbt, String> {
        self.depth += 1;
        if self.depth > 512 {
            self.depth -= 1;
            return Err("NBT too deep".into());
        }
        let r = self.payload_inner(tag);
        self.depth -= 1;
        r
    }

    fn payload_inner(&mut self, tag: u8) -> Result<Nbt, String> {
        Ok(match tag {
            1 => Nbt::Byte(self.u8()? as i8),
            2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?),
            4 => Nbt::Long(self.i64()?),
            5 => Nbt::Float(self.f32()?),
            6 => Nbt::Double(self.f64()?),
            7 => {
                let len = self.i32()?.max(0) as usize;
                Nbt::ByteArray(self.take(len)?.iter().map(|&b| b as i8).collect())
            }
            8 => Nbt::String(self.string()?),
            9 => {
                let elem = self.u8()?;
                let len = self.i32()?.max(0) as usize;
                if elem == 0 && len > 0 {
                    return Err("NBT: non-empty end list".into());
                }
                let mut items = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    items.push(self.payload(elem)?);
                }
                Nbt::List(items)
            }
            10 => {
                let mut fields = Vec::new();
                loop {
                    let child = self.u8()?;
                    if child == 0 {
                        break;
                    }
                    let name = self.string()?;
                    fields.push((name, self.payload(child)?));
                }
                Nbt::Compound(fields)
            }
            11 => {
                let len = self.i32()?.max(0) as usize;
                let mut v = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    v.push(self.i32()?);
                }
                Nbt::IntArray(v)
            }
            12 => {
                let len = self.i32()?.max(0) as usize;
                let mut v = Vec::with_capacity(len.min(4096));
                for _ in 0..len {
                    v.push(self.i64()?);
                }
                Nbt::LongArray(v)
            }
            other => return Err(format!("NBT: unknown tag {other}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str, props: &[(&str, &str)]) -> BlockStateId {
        let block = blocks::block_id_of(name).unwrap_or_else(|| panic!("no block {name}"));
        props::state_with(block, props).map(BlockStateId).unwrap()
    }

    #[test]
    fn transform_matches_vanillas_arithmetic() {
        let p = [0, 0, 0];
        assert_eq!(
            transform([1, 2, 3], Mirror::None, Rotation::None, p),
            [1, 2, 3]
        );
        assert_eq!(
            transform([1, 2, 3], Mirror::LeftRight, Rotation::None, p),
            [1, 2, -3]
        );
        assert_eq!(
            transform([1, 2, 3], Mirror::FrontBack, Rotation::None, p),
            [-1, 2, 3]
        );
        assert_eq!(
            transform([1, 2, 3], Mirror::None, Rotation::Clockwise90, p),
            [-3, 2, 1]
        );
        assert_eq!(
            transform([1, 2, 3], Mirror::None, Rotation::Clockwise180, p),
            [-1, 2, -3]
        );
        assert_eq!(
            transform([1, 2, 3], Mirror::None, Rotation::Counterclockwise90, p),
            [3, 2, -1]
        );
        // A non-zero pivot: the igloo top's pivot is (3, 5, 5).
        let pivot = [3, 5, 5];
        assert_eq!(
            transform([3, 0, 4], Mirror::None, Rotation::Clockwise90, pivot),
            [4, 0, 5]
        );
        assert_eq!(
            transform([3, 0, 4], Mirror::None, Rotation::Clockwise180, pivot),
            [3, 0, 6]
        );
    }

    #[test]
    fn zero_position_and_bounding_box_match_get_zero_position_with_transform() {
        // `StructureTemplate.getZeroPositionWithTransform` for a 3×5×7.
        let base = Pos::new(10, 20, 30);
        assert_eq!(zero_position(base, Rotation::None, [3, 5, 7]), base);
        assert_eq!(
            zero_position(base, Rotation::Clockwise90, [3, 5, 7]),
            base.offset(6, 0, 0)
        );
        assert_eq!(
            zero_position(base, Rotation::Clockwise180, [3, 5, 7]),
            base.offset(2, 0, 6)
        );
        assert_eq!(
            zero_position(base, Rotation::Counterclockwise90, [3, 5, 7]),
            base.offset(0, 0, 2)
        );
        // The rotated bounding box: 3×7 for a quarter turn.
        let (min, max) = bounding_box(base, Rotation::Clockwise90, [3, 5, 7]);
        assert_eq!(min, Pos::new(4, 20, 30));
        assert_eq!(max, Pos::new(10, 24, 32));
    }

    #[test]
    fn rotate_index_wraps_like_the_game() {
        assert_eq!(Rotation::Clockwise90.rotate_index(7, 16), 11);
        assert_eq!(Rotation::Clockwise180.rotate_index(7, 16), 15);
        assert_eq!(Rotation::Counterclockwise90.rotate_index(7, 16), 3);
        assert_eq!(Rotation::None.rotate_index(7, 16), 7);
    }

    #[test]
    fn transform_state_rotates_facing_axis_and_connections() {
        let furnace = state("minecraft:furnace", &[("facing", "east"), ("lit", "false")]);
        let south = state(
            "minecraft:furnace",
            &[("facing", "south"), ("lit", "false")],
        );
        assert_eq!(
            transform_state(furnace, Mirror::None, Rotation::Clockwise90),
            south
        );

        let pillar = state("minecraft:oak_log", &[("axis", "x")]);
        let other = state("minecraft:oak_log", &[("axis", "z")]);
        assert_eq!(
            transform_state(pillar, Mirror::None, Rotation::Clockwise90),
            other
        );

        // Iron bars cross-connections: `new[side] = old[rotate⁻¹(side)]`.
        let bars = state(
            "minecraft:iron_bars",
            &[
                ("north", "true"),
                ("east", "false"),
                ("south", "false"),
                ("west", "true"),
                ("waterlogged", "false"),
            ],
        );
        let rotated = state(
            "minecraft:iron_bars",
            &[
                ("north", "true"),
                ("east", "true"),
                ("south", "false"),
                ("west", "false"),
                ("waterlogged", "false"),
            ],
        );
        assert_eq!(
            transform_state(bars, Mirror::None, Rotation::Clockwise90),
            rotated
        );
    }

    #[test]
    fn a_mirror_swaps_connections() {
        let bars = state(
            "minecraft:iron_bars",
            &[
                ("north", "true"),
                ("east", "false"),
                ("south", "false"),
                ("west", "true"),
                ("waterlogged", "false"),
            ],
        );
        let mirrored = state(
            "minecraft:iron_bars",
            &[
                ("north", "false"),
                ("east", "false"),
                ("south", "true"),
                ("west", "true"),
                ("waterlogged", "false"),
            ],
        );
        assert_eq!(mirror_state(bars, Mirror::LeftRight), mirrored);
    }

    #[test]
    fn the_packs_igloo_templates_parse() {
        let Some(root) = std::env::var("AETHER_VANILLA_PACK").ok() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let tm = TemplateManager::new(&root);
        let top = tm.get("minecraft:igloo/top").expect("igloo/top");
        let middle = tm.get("minecraft:igloo/middle").expect("igloo/middle");
        let bottom = tm.get("minecraft:igloo/bottom").expect("igloo/bottom");
        assert_eq!(top.size, [7, 5, 8]);
        assert_eq!(middle.size, [3, 3, 3]);
        assert_eq!(bottom.size, [7, 6, 9]);
        assert_eq!(top.blocks.len(), 152);
        assert_eq!(middle.blocks.len(), 15);
        assert_eq!(bottom.blocks.len(), 244);
        // The bottom carries the `chest` data marker as a structure block.
        assert!(bottom
            .blocks
            .iter()
            .any(|b| blockinfo::name(b.state) == "minecraft:structure_block"));
    }

    #[test]
    fn load_from_pack_sorts_blocks_in_build_info_list_order() {
        let Some(root) = std::env::var("AETHER_VANILLA_PACK").ok() else {
            eprintln!("skipping: AETHER_VANILLA_PACK not set");
            return;
        };
        let pack = DataPack::open(&root).expect("open pack");
        let t = load_from_pack(&pack, "minecraft:fossil/spine_1").expect("fossil/spine_1");
        assert_eq!(t.size, [3, 3, 13]);
        // `buildInfoList`: sorted by Y, then X, then Z.
        assert!(t
            .blocks
            .windows(2)
            .all(|w| (w[0].pos[1], w[0].pos[0], w[0].pos[2])
                <= (w[1].pos[1], w[1].pos[0], w[1].pos[2])));
    }
}
