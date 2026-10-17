use std::collections::BTreeMap;

use crate::clipboard::Clipboard;

const DATA_VERSION: i32 = 4189;

#[derive(Clone, Debug, PartialEq)]
enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Nbt>),
    Compound(BTreeMap<String, Nbt>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { bytes: Vec::new() }
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn i16(&mut self, value: i16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn string(&mut self, value: &str) {
        self.i16(value.len() as i16);
        self.bytes.extend_from_slice(value.as_bytes());
    }

    fn header(&mut self, tag: u8, name: &str) {
        self.u8(tag);
        self.string(name);
    }

    fn named_short(&mut self, name: &str, value: i16) {
        self.header(2, name);
        self.i16(value);
    }

    fn named_int(&mut self, name: &str, value: i32) {
        self.header(3, name);
        self.i32(value);
    }

    fn named_int_array(&mut self, name: &str, values: &[i32]) {
        self.header(11, name);
        self.i32(values.len() as i32);
        for value in values {
            self.i32(*value);
        }
    }

    fn named_byte_array(&mut self, name: &str, values: &[u8]) {
        self.header(7, name);
        self.i32(values.len() as i32);
        self.bytes.extend_from_slice(values);
    }

    fn named_empty_list(&mut self, name: &str, element: u8) {
        self.header(9, name);
        self.u8(element);
        self.i32(0);
    }

    fn palette(&mut self, name: &str, palette: &BTreeMap<String, i32>) {
        self.header(10, name);
        for (block, index) in palette {
            self.header(3, block);
            self.i32(*index);
        }
        self.u8(0);
    }
}

fn write_varint(out: &mut Vec<u8>, mut value: i32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

pub fn to_bytes(clipboard: &Clipboard) -> Result<Vec<u8>, String> {
    let [width, height, length] = clipboard.size;
    let dimensions = [width, height, length];
    if dimensions.iter().any(|n| *n == 0 || *n > i16::MAX as usize) {
        return Err("schematic dimensions must be between 1 and 32767".to_string());
    }
    if clipboard.blocks.len() != width * height * length {
        return Err("the clipboard is corrupt".to_string());
    }
    let mut palette = BTreeMap::new();
    for block in &clipboard.blocks {
        let next = palette.len() as i32;
        palette.entry(block.clone()).or_insert(next);
    }
    let mut data = Vec::new();
    for y in 0..height {
        for z in 0..length {
            for x in 0..width {
                let index = x + width * (z + length * y);
                write_varint(&mut data, palette[&clipboard.blocks[index]]);
            }
        }
    }

    // Sponge v2 top-level `Offset` is the absolute minimum point, while
    // `Metadata.WEOffset*` is that minimum relative to the clipboard origin.
    // WorldEdit writes both and reads the metadata for relative placement.
    let minimum = [
        clipboard.origin[0] + clipboard.offset[0],
        clipboard.origin[1] + clipboard.offset[1],
        clipboard.origin[2] + clipboard.offset[2],
    ];
    let mut w = Writer::new();
    w.header(10, "Schematic");
    w.named_int("Version", 2);
    w.named_int("DataVersion", DATA_VERSION);
    w.named_short("Width", width as i16);
    w.named_short("Height", height as i16);
    w.named_short("Length", length as i16);
    w.named_int_array("Offset", &minimum);
    w.named_int("PaletteMax", palette.len() as i32);
    w.palette("Palette", &palette);
    w.named_byte_array("BlockData", &data);
    w.named_empty_list("BlockEntities", 10);
    w.header(10, "Metadata");
    w.named_int("WEOffsetX", clipboard.offset[0]);
    w.named_int("WEOffsetY", clipboard.offset[1]);
    w.named_int("WEOffsetZ", clipboard.offset[2]);
    w.u8(0);
    w.u8(0);

    Ok(gzip(&w.bytes))
}

fn gzip(raw: &[u8]) -> Vec<u8> {
    let deflated = miniz_oxide::deflate::compress_to_vec(raw, 6);
    let mut out = Vec::with_capacity(deflated.len() + 18);
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0xff]);
    out.extend_from_slice(&deflated);
    out.extend_from_slice(&crc32(raw).to_le_bytes());
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (!(crc & 1)).wrapping_add(1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn gunzip(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return Err("not a gzip stream".to_string());
    }
    let flags = data[3];
    let mut i = 10usize;
    if flags & 0x04 != 0 {
        if i + 2 > data.len() {
            return Err("truncated gzip header".to_string());
        }
        let extra = u16::from_le_bytes([data[i], data[i + 1]]) as usize;
        i += 2 + extra;
    }
    if flags & 0x08 != 0 {
        while i < data.len() && data[i] != 0 {
            i += 1;
        }
        i += 1;
    }
    if flags & 0x10 != 0 {
        while i < data.len() && data[i] != 0 {
            i += 1;
        }
        i += 1;
    }
    if flags & 0x02 != 0 {
        i += 2;
    }
    let end = data.len().checked_sub(8).ok_or("truncated gzip")?;
    if i > end {
        return Err("truncated gzip body".to_string());
    }
    miniz_oxide::inflate::decompress_to_vec_with_limit(&data[i..end], limit)
        .map_err(|e| format!("cannot inflate schematic: {e:?}"))
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| "truncated schematic NBT".to_string())?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn i16(&mut self) -> Result<i16, String> {
        let bytes = self.take(2)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn i32(&mut self) -> Result<i32, String> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i64(&mut self) -> Result<i64, String> {
        let bytes = self.take(8)?;
        Ok(i64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.i32()? as u32))
    }

    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_bits(self.i64()? as u64))
    }

    fn string(&mut self) -> Result<String, String> {
        let len = self.i16()?;
        if len < 0 {
            return Err("negative NBT string length".to_string());
        }
        let bytes = self.take(len as usize)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "invalid UTF-8 in NBT".to_string())
    }

    /// Check a declared element count against the bytes actually left, so a
    /// crafted NBT header cannot make us pre-allocate gigabytes that the
    /// payload never had.
    fn declared_len(&self, len: i32, stride: usize) -> Result<usize, String> {
        if len < 0 {
            return Err("negative NBT length".to_string());
        }
        let len = len as usize;
        let remaining = self.data.len().saturating_sub(self.pos);
        if len > remaining / stride.max(1) {
            return Err("NBT length exceeds the payload".to_string());
        }
        Ok(len)
    }

    fn value(&mut self, tag: u8) -> Result<Nbt, String> {
        Ok(match tag {
            1 => Nbt::Byte(self.u8()? as i8),
            2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?),
            4 => Nbt::Long(self.i64()?),
            5 => Nbt::Float(self.f32()?),
            6 => Nbt::Double(self.f64()?),
            7 => {
                let len = self.i32()?;
                let len = self.declared_len(len, 1)?;
                Nbt::ByteArray(self.take(len)?.to_vec())
            }
            8 => Nbt::String(self.string()?),
            9 => {
                let element = self.u8()?;
                let len = self.i32()?;
                let len = self.declared_len(len, 1)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(self.value(element)?);
                }
                Nbt::List(values)
            }
            10 => {
                let mut values = BTreeMap::new();
                loop {
                    let tag = self.u8()?;
                    if tag == 0 {
                        break;
                    }
                    let name = self.string()?;
                    let value = self.value(tag)?;
                    values.insert(name, value);
                }
                Nbt::Compound(values)
            }
            11 => {
                let len = self.i32()?;
                let len = self.declared_len(len, 4)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(self.i32()?);
                }
                Nbt::IntArray(values)
            }
            12 => {
                let len = self.i32()?;
                let len = self.declared_len(len, 8)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(self.i64()?);
                }
                Nbt::LongArray(values)
            }
            other => return Err(format!("unsupported NBT tag {other}")),
        })
    }
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<i32, String> {
    let mut value = 0i32;
    for shift in (0..35).step_by(7) {
        let byte = *data
            .get(*pos)
            .ok_or_else(|| "truncated BlockData".to_string())?;
        *pos += 1;
        value |= i32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("invalid BlockData varint".to_string())
}

fn compound<'a>(nbt: &'a Nbt, name: &str) -> Result<&'a BTreeMap<String, Nbt>, String> {
    match nbt {
        Nbt::Compound(map) => Ok(map),
        _ => Err(format!("`{name}` is not a compound")),
    }
}

fn int(map: &BTreeMap<String, Nbt>, name: &str) -> Result<i32, String> {
    match map.get(name) {
        Some(Nbt::Int(value)) => Ok(*value),
        Some(Nbt::Short(value)) => Ok(i32::from(*value)),
        Some(Nbt::Byte(value)) => Ok(i32::from(*value)),
        _ => Err(format!("missing `{name}`")),
    }
}

pub fn from_bytes(bytes: &[u8], max_volume: usize) -> Result<Clipboard, String> {
    // A schematic cell is at least one varint byte, but a large palette and the
    // NBT envelope add overhead. Cap the inflate so a crafted file cannot
    // allocate without bound before the volume limit is even read.
    let limit = max_volume.max(1).saturating_mul(32).saturating_add(1 << 20);
    let raw = gunzip(bytes, limit)?;
    let mut reader = Reader::new(&raw);
    let tag = reader.u8()?;
    if tag != 10 {
        return Err("schematic root is not a compound".to_string());
    }
    let _ = reader.string()?;
    let root = reader.value(10)?;
    let root = compound(&root, "root")?;
    let (palette, data, width, height, length) =
        if let Some(Nbt::Compound(blocks)) = root.get("Blocks") {
            let palette = compound(
                blocks.get("Palette").ok_or("missing `Blocks.Palette`")?,
                "Blocks.Palette",
            )?;
            let data = match blocks.get("Data") {
                Some(Nbt::ByteArray(data)) => data,
                _ => return Err("missing `Blocks.Data`".to_string()),
            };
            (
                palette,
                data,
                int(blocks, "Width")?,
                int(blocks, "Height")?,
                int(blocks, "Length")?,
            )
        } else {
            let palette = compound(root.get("Palette").ok_or("missing `Palette`")?, "Palette")?;
            let data = match root.get("BlockData") {
                Some(Nbt::ByteArray(data)) => data,
                _ => return Err("missing `BlockData`".to_string()),
            };
            (
                palette,
                data,
                int(root, "Width")?,
                int(root, "Height")?,
                int(root, "Length")?,
            )
        };
    if width <= 0 || height <= 0 || length <= 0 {
        return Err("schematic dimensions must be positive".to_string());
    }
    let size = [width as usize, height as usize, length as usize];
    let volume = size[0]
        .checked_mul(size[1])
        .and_then(|n| n.checked_mul(size[2]))
        .ok_or_else(|| "schematic dimensions are too large".to_string())?;
    if volume > max_volume.max(1) {
        return Err(format!(
            "schematic of {volume} blocks exceeds the limit of {max_volume}"
        ));
    }
    let mut names = BTreeMap::new();
    for (name, value) in palette {
        if let Nbt::Int(index) = value {
            names.insert(*index, name.clone());
        }
    }
    let mut blocks = Vec::with_capacity(volume);
    let mut pos = 0;
    for _ in 0..volume {
        let index = read_varint(data, &mut pos)?;
        let name = names
            .get(&index)
            .ok_or_else(|| format!("palette index {index} is missing"))?;
        blocks.push(name.clone());
    }
    if pos != data.len() {
        return Err("BlockData has trailing bytes".to_string());
    }
    // Sponge v2: `Offset` is the absolute minimum, `Metadata.WEOffset*` is the
    // minimum relative to the origin. Prefer the metadata for placement, so a
    // real WorldEdit file lands at the player rather than at its world
    // coordinates; files without metadata keep their historical behaviour.
    let minimum = match root.get("Offset") {
        Some(Nbt::IntArray(values)) if values.len() == 3 => [values[0], values[1], values[2]],
        Some(Nbt::IntArray(values)) => {
            return Err(format!("Offset has {} entries", values.len()));
        }
        Some(_) => return Err("Offset is not an int array".to_string()),
        None => [0, 0, 0],
    };
    let metadata = match root.get("Metadata") {
        Some(nbt @ Nbt::Compound(_)) => Some(compound(nbt, "Metadata")?),
        Some(_) => return Err("Metadata is not a compound".to_string()),
        None => None,
    };
    let relative = metadata.and_then(|metadata| {
        let x = int(metadata, "WEOffsetX").ok()?;
        let y = int(metadata, "WEOffsetY").ok()?;
        let z = int(metadata, "WEOffsetZ").ok()?;
        Some([x, y, z])
    });
    let (origin, offset) = match relative {
        Some(offset) => (
            [
                minimum[0].saturating_sub(offset[0]),
                minimum[1].saturating_sub(offset[1]),
                minimum[2].saturating_sub(offset[2]),
            ],
            offset,
        ),
        None => ([0, 0, 0], minimum),
    };
    Ok(Clipboard {
        world: String::new(),
        origin,
        offset,
        size,
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clipboard() -> Clipboard {
        Clipboard {
            world: "overworld".to_string(),
            origin: [100, 64, -50],
            offset: [1, -2, 3],
            size: [2, 1, 2],
            blocks: vec![
                "minecraft:stone".to_string(),
                "minecraft:air".to_string(),
                "minecraft:oak_stairs[facing=north]".to_string(),
                "minecraft:dirt".to_string(),
            ],
        }
    }

    #[test]
    fn a_clipboard_round_trips_through_gzip_nbt() {
        let original = clipboard();
        let bytes = to_bytes(&original).unwrap();
        assert_eq!(&bytes[..3], &[0x1f, 0x8b, 0x08]);
        let loaded = from_bytes(&bytes, 1_000_000).unwrap();
        assert_eq!(loaded.world, "");
        assert_eq!(loaded.origin, original.origin);
        assert_eq!(loaded.offset, original.offset);
        assert_eq!(loaded.size, original.size);
        assert_eq!(loaded.blocks, original.blocks);

        // Sponge v2 semantics: the top-level Offset is the absolute minimum,
        // the metadata carries the origin-relative offset.
        let raw = gunzip(&bytes, 1 << 20).unwrap();
        let mut reader = Reader::new(&raw);
        assert_eq!(reader.u8().unwrap(), 10);
        let _ = reader.string().unwrap();
        let root = reader.value(10).unwrap();
        let root = compound(&root, "root").unwrap();
        let Some(Nbt::IntArray(minimum)) = root.get("Offset") else {
            panic!("Offset is an int array");
        };
        assert_eq!(minimum, &vec![101, 62, -47]);
        let metadata = compound(root.get("Metadata").unwrap(), "Metadata").unwrap();
        assert_eq!(int(metadata, "WEOffsetX").unwrap(), 1);
        assert_eq!(int(metadata, "WEOffsetY").unwrap(), -2);
        assert_eq!(int(metadata, "WEOffsetZ").unwrap(), 3);
    }

    // A `Schematic` v2 gzip-NBT file shaped like the ones WorldEdit/Sponge
    // writes (absolute top-level `Offset`, origin-relative `Metadata`), built
    // with an external gzip implementation rather than our own writer.
    const WORLDEDIT_FIXTURE: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x4d, 0x8e, 0xdd, 0x4a, 0xc3,
        0x40, 0x10, 0x85, 0x4f, 0xb2, 0xd8, 0x98, 0x2d, 0xa2, 0xcf, 0xe0, 0x63, 0x14, 0x84, 0x52,
        0x5a, 0xe8, 0x85, 0x45, 0x41, 0xb0, 0xfe, 0x50, 0xca, 0xb0, 0x99, 0x24, 0x43, 0xdb, 0x5d,
        0xc8, 0xce, 0x85, 0x4f, 0xe9, 0x2b, 0xc5, 0x4d, 0x2a, 0xd5, 0x8b, 0x81, 0x39, 0x1f, 0x73,
        0xce, 0x1c, 0x8b, 0xf2, 0xc5, 0xb5, 0x7c, 0x22, 0x15, 0x67, 0x50, 0xbc, 0x72, 0x17, 0x25,
        0x78, 0x00, 0xb9, 0xc1, 0x74, 0x49, 0x4a, 0x17, 0x72, 0xb7, 0xcb, 0x71, 0xb5, 0x95, 0x4a,
        0x5b, 0xe4, 0x39, 0x26, 0x6b, 0x96, 0xa6, 0x55, 0x64, 0x69, 0x7d, 0x64, 0xdf, 0x24, 0x9a,
        0x4d, 0x31, 0x79, 0xaa, 0xeb, 0xc8, 0x9a, 0xec, 0x26, 0x4d, 0x95, 0x66, 0xde, 0xf7, 0xfd,
        0xb7, 0x81, 0x7d, 0xa6, 0x23, 0xab, 0xf2, 0x86, 0xbe, 0x86, 0x6c, 0x8b, 0xe2, 0x17, 0x18,
        0xdc, 0x9e, 0xc4, 0xb3, 0xeb, 0xa8, 0xd6, 0x59, 0xd4, 0xe0, 0x19, 0xa3, 0xfb, 0xfe, 0x8f,
        0x06, 0x3a, 0xec, 0xa3, 0x92, 0x74, 0xf1, 0xb3, 0x26, 0x27, 0xbe, 0x79, 0xf0, 0xa1, 0xd3,
        0x76, 0x97, 0xee, 0x32, 0x14, 0x28, 0x17, 0xc7, 0xe0, 0x0e, 0x43, 0xd5, 0x21, 0x19, 0x59,
        0x89, 0x9b, 0x91, 0xac, 0xbc, 0x8a, 0x0a, 0x47, 0x3b, 0x04, 0x5a, 0x5c, 0x6f, 0x58, 0xa9,
        0x4a, 0x57, 0x06, 0xe5, 0x76, 0x75, 0xee, 0xf9, 0x96, 0xca, 0xf5, 0xff, 0xf4, 0xfb, 0xf9,
        0xf7, 0x45, 0x7f, 0x8c, 0x89, 0xf8, 0x01, 0xb8, 0x74, 0xf1, 0x7c, 0x24, 0x01, 0x00, 0x00,
    ];

    #[test]
    fn a_worldedit_file_uses_the_metadata_for_relative_placement() {
        let loaded = from_bytes(WORLDEDIT_FIXTURE, 1_000_000).unwrap();
        assert_eq!(loaded.origin, [101, 64, -52]);
        assert_eq!(loaded.offset, [-1, 0, 2]);
        assert_eq!(loaded.size, [2, 1, 1]);
        assert_eq!(
            loaded.blocks,
            vec![
                "minecraft:stone".to_string(),
                "minecraft:oak_stairs[facing=north]".to_string()
            ]
        );
    }

    #[test]
    fn an_oversized_schematic_is_refused_before_allocating_its_cells() {
        let mut palette = BTreeMap::new();
        palette.insert("minecraft:stone".to_string(), 0);
        let mut w = Writer::new();
        w.header(10, "Schematic");
        w.named_short("Width", 100);
        w.named_short("Height", 100);
        w.named_short("Length", 100);
        w.named_int_array("Offset", &[0, 0, 0]);
        w.palette("Palette", &palette);
        w.named_byte_array("BlockData", &[]);
        w.u8(0);
        let bytes = gzip(&w.bytes);
        let err = from_bytes(&bytes, 10).unwrap_err();
        assert!(err.contains("exceeds the limit"), "{err}");
    }

    #[test]
    fn a_declared_length_larger_than_the_payload_is_refused_before_allocating() {
        let mut w = Writer::new();
        w.header(10, "Schematic");
        w.header(11, "Offset");
        w.i32(1_000_000_000);
        w.u8(0);
        let bytes = gzip(&w.bytes);
        let err = from_bytes(&bytes, 1_000_000).unwrap_err();
        assert!(err.contains("payload"), "{err}");
    }

    #[test]
    fn a_decompression_bomb_is_capped() {
        let raw = vec![0u8; 2 << 20];
        let bytes = gzip(&raw);
        let err = from_bytes(&bytes, 1).unwrap_err();
        assert!(err.contains("inflate"), "{err}");
    }

    #[test]
    fn a_non_gzip_file_is_refused() {
        assert!(from_bytes(b"not a schematic", 1_000_000).is_err());
        assert!(from_bytes(&[], 1_000_000).is_err());
    }

    #[test]
    fn a_truncated_payload_is_refused() {
        let mut bytes = to_bytes(&clipboard()).unwrap();
        bytes.truncate(bytes.len() - 20);
        assert!(from_bytes(&bytes, 1_000_000).is_err());
    }
}
