//! Structure templates — the `data/minecraft/structure/*.nbt` files features
//! place (only `fossil` in the overworld), and the processors the pack defines
//! for them.
//!
//! The templates are gzipped NBT, read from the operator's own copy of the
//! game. Placement is a port of `StructureTemplate.placeInWorld` cut down to
//! what these templates use: no entities, no block entities, no fluid
//! post-processing. The three processors the fossil lists reference
//! (`block_rot`, `rule`, `protected_blocks`) are ported in full, including
//! their random draw order, because that order is part of the feature seed.

use aether_world::BlockStateId;

use super::density::{BuildError, DataPack};
use super::feature::blocks::RuleTest;
use super::feature::level::{Dir, Level, Pos};
use super::json::Json;
use super::rng::{Rng, WorldgenRandom};
use super::surface::parse_block_state;
use super::tags::{BlockSet, Tags};

// --- NBT -------------------------------------------------------------------

/// The NBT subset structure files use.
#[derive(Debug, Clone)]
pub enum Nbt {
    /// `TAG_Byte`.
    Byte(i8),
    /// `TAG_Short`.
    Short(i16),
    /// `TAG_Int`.
    Int(i32),
    /// `TAG_Long`.
    Long(i64),
    /// `TAG_Float`.
    Float(f32),
    /// `TAG_Double`.
    Double(f64),
    /// `TAG_Byte_Array`.
    ByteArray(Vec<u8>),
    /// `TAG_String`.
    String(String),
    /// `TAG_List`.
    List(Vec<Nbt>),
    /// `TAG_Compound`.
    Compound(Vec<(String, Nbt)>),
    /// `TAG_Int_Array`.
    IntArray(Vec<i32>),
    /// `TAG_Long_Array`.
    LongArray(Vec<i64>),
}

impl Nbt {
    /// A compound field.
    pub fn get(&self, key: &str) -> Option<&Nbt> {
        match self {
            Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    /// An integer-valued field.
    pub fn int(&self) -> Option<i32> {
        Some(match self {
            Nbt::Byte(v) => *v as i32,
            Nbt::Short(v) => *v as i32,
            Nbt::Int(v) => *v,
            _ => return None,
        })
    }
    /// A string-valued field.
    pub fn string(&self) -> Option<&str> {
        match self {
            Nbt::String(s) => Some(s),
            _ => None,
        }
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.buf.get(self.at)?;
        self.at += 1;
        Some(v)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let v = self.buf.get(self.at..self.at + n)?;
        self.at += n;
        Some(v)
    }
    fn i16(&mut self) -> Option<i16> {
        Some(i16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<String> {
        let n = self.i16()? as usize;
        Some(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn payload(&mut self, tag: u8) -> Option<Nbt> {
        Some(match tag {
            1 => Nbt::Byte(self.u8()? as i8),
            2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?),
            4 => Nbt::Long(self.i64()?),
            5 => Nbt::Float(self.f32()?),
            6 => Nbt::Double(self.f64()?),
            7 => {
                let n = self.i32()? as usize;
                Nbt::ByteArray(self.take(n)?.to_vec())
            }
            8 => Nbt::String(self.string()?),
            9 => {
                let item = self.u8()?;
                let n = self.i32()? as usize;
                let mut v = Vec::with_capacity(n.min(1 << 20));
                for _ in 0..n {
                    v.push(self.payload(item)?);
                }
                Nbt::List(v)
            }
            10 => {
                let mut fields = Vec::new();
                loop {
                    let t = self.u8()?;
                    if t == 0 {
                        break;
                    }
                    let name = self.string()?;
                    fields.push((name, self.payload(t)?));
                }
                Nbt::Compound(fields)
            }
            11 => {
                let n = self.i32()? as usize;
                let mut v = Vec::with_capacity(n.min(1 << 20));
                for _ in 0..n {
                    v.push(self.i32()?);
                }
                Nbt::IntArray(v)
            }
            12 => {
                let n = self.i32()? as usize;
                let mut v = Vec::with_capacity(n.min(1 << 20));
                for _ in 0..n {
                    v.push(self.i64()?);
                }
                Nbt::LongArray(v)
            }
            _ => return None,
        })
    }
}

/// Parse a whole NBT document (uncompressed).
pub fn parse_nbt(buf: &[u8]) -> Option<Nbt> {
    let mut c = Cursor { buf, at: 0 };
    let tag = c.u8()?;
    if tag != 10 {
        return None;
    }
    let _name = c.string()?;
    c.payload(10)
}

/// Gunzip, the way the pack's structure files are stored.
fn gunzip(buf: &[u8]) -> Option<Vec<u8>> {
    if buf.len() < 18 || buf[0] != 0x1f || buf[1] != 0x8b {
        return None;
    }
    let flg = buf[3];
    let mut i = 10;
    if flg & 0x04 != 0 {
        let xlen = u16::from_le_bytes([*buf.get(i)?, *buf.get(i + 1)?]) as usize;
        i += 2 + xlen;
    }
    if flg & 0x08 != 0 {
        while *buf.get(i)? != 0 {
            i += 1;
        }
        i += 1;
    }
    if flg & 0x10 != 0 {
        while *buf.get(i)? != 0 {
            i += 1;
        }
        i += 1;
    }
    if flg & 0x02 != 0 {
        i += 2;
    }
    let end = buf.len().checked_sub(8)?;
    miniz_oxide::inflate::decompress_to_vec(buf.get(i..end)?).ok()
}

// --- rotation --------------------------------------------------------------

/// `Rotation`, in the game's declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// `none`.
    None,
    /// `clockwise_90`.
    Cw90,
    /// `180`.
    Cw180,
    /// `counterclockwise_90`.
    Ccw90,
}

impl Rotation {
    /// `Rotation.getRandom`.
    pub fn random(r: &mut impl Rng) -> Self {
        match r.next_int_bounded(4) {
            1 => Rotation::Cw90,
            2 => Rotation::Cw180,
            3 => Rotation::Ccw90,
            _ => Rotation::None,
        }
    }

    /// `blockState.rotate`: the state's horizontal direction, rotated.
    pub fn rotate_state(self, s: BlockStateId) -> BlockStateId {
        use super::blockinfo::{prop, with_prop};
        let mut s = s;
        match self {
            Rotation::None => {}
            Rotation::Cw90 | Rotation::Ccw90 => {
                if let Some(ax) = prop(s, "axis") {
                    let n = match ax {
                        "x" => "z",
                        "z" => "x",
                        o => o,
                    };
                    s = with_prop(s, "axis", n);
                }
                if let Some(d) = prop(s, "facing").and_then(Dir::parse) {
                    let n = match self {
                        Rotation::Cw90 => d.clockwise(),
                        _ => d.clockwise().clockwise().clockwise(),
                    };
                    s = with_prop(s, "facing", n.name());
                }
            }
            Rotation::Cw180 => {
                if let Some(d) = prop(s, "facing").and_then(Dir::parse) {
                    s = with_prop(s, "facing", d.opposite().name());
                }
            }
        }
        s
    }

    /// `StructureTemplate.transform(pos, NONE, rotation, ZERO)`.
    fn transform(self, p: Pos) -> Pos {
        match self {
            Rotation::None => p,
            Rotation::Cw90 => Pos::new(-p.z, p.y, p.x),
            Rotation::Cw180 => Pos::new(-p.x, p.y, -p.z),
            Rotation::Ccw90 => Pos::new(p.z, p.y, -p.x),
        }
    }
}

/// `StructureTemplate.getZeroPositionWithTransform(pos, NONE, rotation)` for
/// the template's original size.
pub fn zero_position(base: Pos, rotation: Rotation, size: [i32; 3]) -> Pos {
    let (i, j) = (size[0] - 1, size[2] - 1);
    match rotation {
        Rotation::None => base,
        Rotation::Cw90 => base.offset(j, 0, 0),
        Rotation::Cw180 => base.offset(i, 0, j),
        Rotation::Ccw90 => base.offset(0, 0, i),
    }
}

/// The template's placement bounding box for a rotation, as `(min, max)`.
pub fn bounding_box(base: Pos, rotation: Rotation, size: [i32; 3]) -> (Pos, Pos) {
    let a = Pos::new(0, 0, 0);
    let b = rotation.transform(Pos::new(size[0] - 1, size[1] - 1, size[2] - 1));
    let min = Pos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)).offset(base.x, base.y, base.z);
    let max = Pos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)).offset(base.x, base.y, base.z);
    (min, max)
}

// --- templates -------------------------------------------------------------

/// One block of a template.
#[derive(Debug, Clone)]
pub struct TemplateBlock {
    /// Position within the template.
    pub pos: Pos,
    /// Its state.
    pub state: BlockStateId,
}

/// A structure template.
#[derive(Debug, Clone)]
pub struct StructureTemplate {
    /// The template's size.
    pub size: [i32; 3],
    /// Its blocks, in the game's palette order (`Y`, then `X`, then `Z`).
    pub blocks: Vec<TemplateBlock>,
}

fn state_from_nbt(j: &Nbt) -> Option<BlockStateId> {
    let name = j.get("Name")?.string()?;
    let mut s = name.to_string();
    if let Some(Nbt::Compound(props)) = j.get("Properties") {
        let parts: Vec<String> = props
            .iter()
            .filter_map(|(k, v)| v.string().map(|v| format!("{k}={v}")))
            .collect();
        s = format!("{name}[{}]", parts.join(","));
    }
    super::blockinfo::parse_state(&s)
}

/// Read a structure template from its gzipped NBT bytes.
pub fn load(bytes: &[u8]) -> Option<StructureTemplate> {
    let raw = gunzip(bytes)?;
    let root = parse_nbt(&raw)?;
    let size_v = match root.get("size")? {
        Nbt::IntArray(a) => a.clone(),
        Nbt::List(l) => l.iter().map(Nbt::int).collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    if size_v.len() != 3 {
        return None;
    }
    let size = [size_v[0], size_v[1], size_v[2]];
    let palette: Vec<BlockStateId> = match root.get("palette")? {
        Nbt::List(l) => l.iter().map(state_from_nbt).collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    let mut blocks = Vec::new();
    if let Some(Nbt::List(l)) = root.get("blocks") {
        for b in l {
            let pos = match b.get("pos")? {
                Nbt::IntArray(a) if a.len() == 3 => Pos::new(a[0], a[1], a[2]),
                Nbt::List(a) if a.len() == 3 => Pos::new(a[0].int()?, a[1].int()?, a[2].int()?),
                _ => return None,
            };
            let idx = b.get("state")?.int()? as usize;
            blocks.push(TemplateBlock {
                pos,
                state: *palette.get(idx)?,
            });
        }
    }
    // `buildInfoList`: solid blocks sorted by Y, then X, then Z.
    blocks.sort_by_key(|b| (b.pos.y, b.pos.x, b.pos.z));
    Some(StructureTemplate { size, blocks })
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
            RuleLocation::Block(b) => super::blockinfo::block_of(lv.get(p)) == *b,
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
        integrity: f32,
        rottable: Option<BlockSet>,
    },
    /// `rule`.
    Rule(Vec<Rule>),
    /// `protected_blocks`.
    Protected { cannot: BlockSet },
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
                let mut pos_r = WorldgenRandom::legacy(super::mth::get_seed(pos.x, pos.y, pos.z));
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
                        .and_then(parse_block_state)
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

// --- placement -------------------------------------------------------------

/// `StructureTemplate.placeInWorld`, cut to what fossil templates use.
pub fn place_in_world(
    lv: &mut Level,
    origin: Pos,
    template: &StructureTemplate,
    rotation: Rotation,
    processors: &[Processor],
    bbox: Option<(Pos, Pos)>,
    r: &mut WorldgenRandom,
) {
    // `StructurePlaceSettings.getRandomPalette`: a single-palette template
    // still draws `nextInt(1)`, so the world random advances here even though
    // the result is always palette 0.
    let _ = r.next_int_bounded(1);
    let mut processed: Vec<(Pos, BlockStateId)> = Vec::new();
    for b in &template.blocks {
        let pos = rotation
            .transform(b.pos)
            .offset(origin.x, origin.y, origin.z);
        let mut state = Some(b.state);
        for p in processors {
            match state.and_then(|s| p.process(lv, pos, s, r)) {
                Some(s) => state = Some(s),
                None => {
                    state = None;
                    break;
                }
            }
        }
        if let Some(s) = state {
            processed.push((pos, s));
        }
    }
    for (pos, s) in processed {
        if let Some((min, max)) = bbox {
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
        lv.set(pos, rotation.rotate_state(s));
    }
}

/// Read a structure template from the pack.
pub fn load_from_pack(pack: &DataPack, id: &str) -> Result<StructureTemplate, BuildError> {
    let bytes = pack.read_structure(id)?;
    load(&bytes).ok_or_else(|| BuildError::new(format!("cannot parse structure `{id}`")))
}
