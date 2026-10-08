//! Chunk encoding for protocol 768 (1.21.2/1.21.3): paletted containers and
//! bit-packed long arrays.
//!
//! A section is a `short` non-air count followed by two paletted containers —
//! blocks then biomes — and the light arrives in the same packet, indexed by a
//! bit set of which sections carry it.
//!
//! Values in a paletted container never straddle a `long` boundary: each
//! `long` holds `64 / bits_per_entry` whole entries and the leftover high bits
//! go unused.

use super::super::nbt;
use super::super::BlockSource;
use super::block_state;
use super::registry::{MIN_Y, SECTIONS};
use crate::proto::PacketOut;

/// Blocks per section along each axis.
const AXIS: i32 = 16;
/// Entries in one section: `16³`.
const ENTRIES: usize = 4096;
/// Bits per block index. The engine has ten blocks, so a 4-bit indirect
/// palette — the format's minimum for blocks — always suffices.
const BLOCK_BITS: u8 = 4;

/// Pack `values` (indices into a palette) into the protocol's long array.
fn pack(values: &[u32], bits: u8) -> Vec<i64> {
    let per_long = 64 / bits as usize;
    let mut longs = Vec::with_capacity(values.len().div_ceil(per_long));
    for group in values.chunks(per_long) {
        let mut acc: u64 = 0;
        for (i, v) in group.iter().enumerate() {
            acc |= (*v as u64) << (i * bits as usize);
        }
        longs.push(acc as i64);
    }
    longs
}

fn write_varint(out: &mut Vec<u8>, v: i32) {
    let mut u = v as u32;
    loop {
        if u & !0x7f == 0 {
            out.push(u as u8);
            return;
        }
        out.push(((u & 0x7f) | 0x80) as u8);
        u >>= 7;
    }
}

/// Write a paletted container holding one repeated value (`bits = 0`).
fn write_single_valued(out: &mut Vec<u8>, value: u32) {
    out.push(0); // bits per entry: 0 means "the whole container is this value"
    write_varint(out, value as i32);
    write_varint(out, 0); // an empty data array still carries its length
}

/// Write an indirect paletted container.
///
/// This version still length-prefixes the data array. The prefix was
/// removed in 1.21.5, so writing it there desynchronises the column.
fn write_indirect(out: &mut Vec<u8>, palette: &[u32], indices: &[u32]) {
    out.push(BLOCK_BITS);
    write_varint(out, palette.len() as i32);
    for v in palette {
        write_varint(out, *v as i32);
    }
    let longs = pack(indices, BLOCK_BITS);
    write_varint(out, longs.len() as i32);
    for l in &longs {
        out.extend_from_slice(&l.to_be_bytes());
    }
}

/// Encode one section's containers, returning the non-air block count.
fn write_section(out: &mut Vec<u8>, states: &[u32; ENTRIES]) -> i16 {
    let non_air = states.iter().filter(|s| **s != 0).count() as i16;

    // Palette in first-seen order.
    let mut palette: Vec<u32> = Vec::new();
    let mut indices = Vec::with_capacity(ENTRIES);
    for s in states.iter() {
        let idx = match palette.iter().position(|p| p == s) {
            Some(i) => i,
            None => {
                palette.push(*s);
                palette.len() - 1
            }
        };
        indices.push(idx as u32);
    }

    out.extend_from_slice(&non_air.to_be_bytes());
    if palette.len() == 1 {
        write_single_valued(out, palette[0]);
    } else {
        write_indirect(out, &palette, &indices);
    }
    // Biomes: one biome everywhere, so a single-valued container. The id is a
    // registry index, and this server registers exactly one biome.
    write_single_valued(out, 0);
    non_air
}

/// Build the Chunk Data and Update Light packet for column `(cx, cz)`.
pub fn chunk_data_packet(id: i32, cx: i32, cz: i32, world: &dyn BlockSource) -> PacketOut {
    let mut column = Vec::new();
    for sy in 0..SECTIONS {
        let mut states = [0u32; ENTRIES];
        for (i, slot) in states.iter_mut().enumerate() {
            let x = (i & 15) as i32;
            let z = ((i >> 4) & 15) as i32;
            let y = ((i >> 8) & 15) as i32;
            let wy = MIN_Y + sy * AXIS + y;
            *slot = block_state(world.block_at(cx * AXIS + x, wy, cz * AXIS + z));
        }
        write_section(&mut column, &states);
    }

    let mut p = PacketOut::new(id);
    p.i32(cx).i32(cz);

    // Heightmaps: an anonymous NBT compound in this version.
    p.bytes(&nbt::compound([]).to_network());

    p.var_int(column.len() as i32).bytes(&column);
    p.var_int(0); // no block entities

    // Full-bright: every section, plus the void section below and above,
    // carries maximum sky and block light.
    let light_sections = SECTIONS + 2;
    let mask = (1i64 << light_sections) - 1;
    for _ in 0..2 {
        p.var_int(1).i64(mask); // sky mask, then block mask
    }
    for _ in 0..2 {
        p.var_int(1).i64(0); // empty sky mask, then empty block mask
    }
    for _ in 0..2 {
        p.var_int(light_sections);
        for _ in 0..light_sections {
            p.var_int(2048).bytes(&[0xFF; 2048]);
        }
    }
    p
}

/// Unload column `(cx, cz)`.
pub fn unload_chunk_packet(id: i32, cx: i32, cz: i32) -> PacketOut {
    // Note the order: this packet carries Z **before** X, unlike every other
    // chunk packet in the protocol.
    let mut p = PacketOut::new(id);
    p.i32(cz).i32(cx);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::block_ids as b;
    use aether_world::BlockStateId;

    /// A world of nothing but air.
    pub struct Empty;
    impl BlockSource for Empty {
        fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
            BlockStateId::AIR
        }
    }

    struct Flat;
    impl BlockSource for Flat {
        fn block_at(&self, _x: i32, y: i32, _z: i32) -> BlockStateId {
            match y {
                0 => b::BEDROCK,
                1..=30 => b::STONE,
                31 => b::GRASS_BLOCK,
                _ => b::AIR,
            }
        }
    }

    fn read_varint(b: &[u8], pos: &mut usize) -> i32 {
        let mut v = 0i32;
        for i in 0..5 {
            let byte = b[*pos];
            *pos += 1;
            v |= ((byte & 0x7f) as i32) << (7 * i);
            if byte & 0x80 == 0 {
                break;
            }
        }
        v
    }

    /// Read back a paletted container the way this version's client does.
    ///
    /// Written from the format description rather than mirrored off the
    /// encoder: an encoder and a decoder that share a mistake agree with each
    /// other and pass, which is exactly how the stray length prefix survived.
    fn read_container(b: &[u8], pos: &mut usize) -> Vec<u32> {
        let bits = b[*pos];
        *pos += 1;
        if bits == 0 {
            let value = read_varint(b, pos) as u32;
            assert_eq!(read_varint(b, pos), 0, "single-valued carries no data");
            return vec![value; ENTRIES];
        }
        let palette_len = read_varint(b, pos);
        let palette: Vec<u32> = (0..palette_len)
            .map(|_| read_varint(b, pos) as u32)
            .collect();
        let longs = read_varint(b, pos) as usize;
        let per_long = 64 / bits as usize;
        let mut out = Vec::with_capacity(ENTRIES);
        for _ in 0..longs {
            let l = u64::from_be_bytes(b[*pos..*pos + 8].try_into().unwrap());
            *pos += 8;
            for i in 0..per_long {
                if out.len() == ENTRIES {
                    break;
                }
                let idx = ((l >> (i * bits as usize)) & ((1 << bits) - 1)) as usize;
                out.push(palette[idx]);
            }
        }
        out
    }

    #[test]
    fn packing_never_straddles_a_long_boundary() {
        let values: Vec<u32> = (0..16).collect();
        let longs = pack(&values, 4);
        assert_eq!(longs.len(), 1);
        let l = longs[0] as u64;
        for (i, v) in values.iter().enumerate() {
            assert_eq!((l >> (i * 4)) & 0xF, *v as u64, "entry {i}");
        }
    }

    #[test]
    fn section_round_trips_through_the_client_view() {
        let world = Flat;
        let mut states = [0u32; ENTRIES];
        for (i, slot) in states.iter_mut().enumerate() {
            let x = (i & 15) as i32;
            let z = ((i >> 4) & 15) as i32;
            let y = ((i >> 8) & 15) as i32;
            *slot = block_state(world.block_at(x, y + 16, z));
        }
        let mut buf = Vec::new();
        let count = write_section(&mut buf, &states);

        let mut pos = 2usize;
        assert_eq!(i16::from_be_bytes([buf[0], buf[1]]), count);
        assert_eq!(read_container(&buf, &mut pos), states.to_vec());
    }

    #[test]
    fn palette_data_array_is_length_prefixed() {
        // Removed in 1.21.5; this version predates that, so the prefix must be
        // on the wire or every field after it shifts.
        let mut buf = Vec::new();
        write_single_valued(&mut buf, 7);
        assert_eq!(buf, vec![0, 7, 0], "bits, value, then a zero length");
    }

    #[test]
    fn heightmaps_are_an_nbt_compound() {
        // An array here (the 1.21.5 shape) would shift every following field.
        let mut wire = Vec::new();
        chunk_data_packet(0x27, 0, 0, &Empty)
            .write_to(&mut wire, None)
            .unwrap();
        let mut pos = 0usize;
        read_varint(&wire, &mut pos);
        read_varint(&wire, &mut pos);
        pos += 8;
        assert_eq!(&wire[pos..pos + 2], &[0x0A, 0x00], "empty NBT compound");
    }

    #[test]
    fn unload_packet_puts_z_before_x() {
        let mut wire = Vec::new();
        unload_chunk_packet(0x21, 3, -5)
            .write_to(&mut wire, None)
            .unwrap();
        let body = &wire[2..];
        assert_eq!(i32::from_be_bytes(body[0..4].try_into().unwrap()), -5);
        assert_eq!(i32::from_be_bytes(body[4..8].try_into().unwrap()), 3);
    }
}
