//! Structure templates: the `.nbt` files under `data/minecraft/structure/`.
//!
//! A template is a palette plus a list of `(pos, state)` blocks; placing it
//! transforms each block by a mirror, a rotation about a rotation pivot, and a
//! world position, then writes the rotated block state. This is a port of the
//! parts of `StructureTemplate` a data-driven structure needs:
//!
//! * [`load`](Template::load) — parse the (gzipped) NBT: `size`, `palette`,
//!   `blocks` (and the first of `palettes`, when a template has several);
//! * [`transform`] — `StructureTemplate.transform`;
//! * [`transform_state`] — `BlockState.mirror(mirror).rotate(rotation)` for the
//!   property shapes the overworld structures use (directional `facing`, the
//!   `axis` of a pillar, the four cross-connection booleans, a 16-way
//!   `rotation`); anything else is left as it was.
//! * [`Template::place`] — the writes, in template order, with
//!   `structure_block` markers dropped (`BlockIgnoreProcessor`). A marker in
//!   `data` mode leaves air behind, as `TemplateStructurePiece`'s
//!   `handleDataMarker` does for the igloo's chest marker.
//!
//! `TemplateManager` reads and caches templates from the operator's own data
//! pack, never vendoring them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

use crate::vanilla::blockinfo;

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
pub struct Template {
    /// `size`.
    pub size: [i32; 3],
    /// Every block, in the order the file lists them.
    pub blocks: Vec<TemplateBlock>,
}

impl Template {
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
            palette.push(parse_block_state(p)?);
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

    /// Place the template at `position`, applying `mirror` then `rotation`
    /// about `pivot`. Returns the world writes; `structure_block` data markers
    /// become air (as `handleDataMarker` leaves them) and every other
    /// `structure_block` is dropped, as `BlockIgnoreProcessor.STRUCTURE_BLOCK`
    /// does.
    pub fn place(
        &self,
        position: [i32; 3],
        rotation: Rotation,
        mirror: Mirror,
        pivot: [i32; 3],
    ) -> Vec<(i32, i32, i32, BlockStateId)> {
        let mut out = Vec::with_capacity(self.blocks.len());
        for b in &self.blocks {
            let r = transform(b.pos, mirror, rotation, pivot);
            let mut push = |state: BlockStateId| {
                out.push((
                    position[0] + r[0],
                    position[1] + r[1],
                    position[2] + r[2],
                    state,
                ));
            };
            if blockinfo::name(b.state) == "minecraft:structure_block" {
                if is_data_marker(b.state) {
                    push(BlockStateId::AIR);
                }
                continue;
            }
            push(transform_state(b.state, mirror, rotation));
        }
        out
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

fn parse_block_state(j: &Nbt) -> Result<BlockStateId, String> {
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
    let l = j?.as_list()?;
    Some([
        l.first()?.as_i64()? as i32,
        l.get(1)?.as_i64()? as i32,
        l.get(2)?.as_i64()? as i32,
    ])
}

/// Reads and caches templates from a data pack's `data/minecraft/structure/`.
pub struct TemplateManager {
    dir: PathBuf,
    cache: Mutex<HashMap<String, Arc<Template>>>,
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
    pub fn get(&self, id: &str) -> Option<Arc<Template>> {
        let rel = id.strip_prefix("minecraft:").unwrap_or(id);
        if let Some(t) = self.cache.lock().unwrap().get(rel) {
            return Some(Arc::clone(t));
        }
        let path = self.dir.join(format!("{rel}.nbt"));
        let data = std::fs::read(&path).ok()?;
        let t = Arc::new(Template::from_gzip(&data).ok()?);
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
    fn the_packs_igloo_templates_parse_and_place() {
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

        // No structure block survives as a write; the bottom's `chest` data
        // marker becomes air.
        for t in [&top, &middle, &bottom] {
            let writes = t.place([0, 0, 0], Rotation::None, Mirror::None, [0, 0, 0]);
            assert!(writes
                .iter()
                .all(|(_, _, _, s)| blockinfo::name(*s) != "minecraft:structure_block"));
        }
        let writes = bottom.place([0, 0, 0], Rotation::None, Mirror::None, [0, 0, 0]);
        assert_eq!(writes.len(), 244);
        // The marker sat at (1, 2, 6); with no pivot it lands there as air.
        assert!(writes
            .iter()
            .any(|&(x, y, z, s)| (x, y, z) == (1, 2, 6) && s == BlockStateId::AIR));
    }
}
