//! Chunk encoding for 1.19.2: paletted containers and bit-packed longs.
//!
//! A section is a `short` non-air count followed by a block container and a
//! biome container, with light in the same packet indexed by `BitSet`s. Two
//! details of this release desynchronise the whole packet if missed:
//!
//! * the heightmaps NBT uses the **named** root form (see
//!   [`super::registry::named_root`]); the anonymous root starts at 1.20.2;
//! * a `trustEdges` boolean sits between the block entities and the light
//!   masks. It was dropped in 1.20, so it is written here but not by the
//!   1.20.1 module.
//!
//! The paletted container's long array **is** length-prefixed here. That
//! prefix was removed in 1.21.5, so the 1.21 modules omit it — do not copy
//! their layout back into this generation.
//!
//! Values never straddle a `long` boundary: each `long` holds
//! `64 / bits_per_entry` whole entries and the leftover high bits go unused.

use super::super::nbt::Nbt;
use super::super::BlockSource;
use super::block_state;
use super::registry::{named_root, MIN_Y, SECTIONS};
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
    write_varint(out, 0); // data array length: none follows
}

/// Write an indirect paletted container.
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

    // Build the palette in first-seen order.
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
    // Biomes: one biome everywhere, so a single-valued container holding the
    // only registry index this server registers.
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

    // Heightmaps: an empty compound. The client uses them for particle and
    // spawn heuristics, not rendering, and recomputes what it needs — but the
    // field is not optional, and here it is a *named* root.
    p.bytes(&named_root(&Nbt::Compound(vec![])));

    p.var_int(column.len() as i32).bytes(&column);
    p.var_int(0); // no block entities

    // Present up to 1.19.4; 1.20 dropped it.
    p.bool(true);

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
    let mut p = PacketOut::new(id);
    p.i32(cx).i32(cz);
    p
}

/// A stand-in world for tests in sibling modules that encode non-chunk events
/// but still have to satisfy the [`BlockSource`] parameter.
#[cfg(test)]
pub mod tests_support {
    use super::BlockSource;
    use aether_world::BlockStateId;

    /// A world of nothing but air.
    pub struct Empty;
    impl BlockSource for Empty {
        fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
            BlockStateId::AIR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::block_ids as b;
    use aether_world::BlockStateId;

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

    /// Read back a paletted container as a client of this version does.
    ///
    /// Written from the format description rather than by mirroring the
    /// encoder: an encoder and a decoder that share a wrong assumption agree
    /// with each other and hide the bug.
    fn read_container(b: &[u8], pos: &mut usize) -> Vec<u32> {
        let read_varint = |b: &[u8], pos: &mut usize| -> i32 {
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
        };
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
        // Length-prefixed in this generation; 1.21.5 removed the prefix.
        let longs = read_varint(b, pos) as usize;
        let per_long = 64 / bits as usize;
        assert_eq!(
            longs,
            ENTRIES.div_ceil(per_long),
            "declared long count must match the entry count"
        );
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
        // Encode a mixed section and decode it exactly as a client would,
        // requiring every block to land where it was generated.
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

        let mut pos = 2usize; // past the non-air short
        assert_eq!(i16::from_be_bytes([buf[0], buf[1]]), count);
        assert_eq!(read_container(&buf, &mut pos), states.to_vec());
        // The biome container follows and must also be readable.
        assert_eq!(read_container(&buf, &mut pos), vec![0u32; ENTRIES]);
        assert_eq!(pos, buf.len(), "section must be consumed exactly");
    }

    #[test]
    fn uniform_section_uses_the_single_valued_form() {
        let states = [0u32; ENTRIES];
        let mut buf = Vec::new();
        assert_eq!(write_section(&mut buf, &states), 0, "no non-air blocks");
        assert_eq!(buf[2], 0, "bits per entry must be 0");
    }

    #[test]
    fn trust_edges_byte_is_present() {
        // The one byte that shifts every light mask if it goes missing. Its
        // presence is checked by length against a packet built without it.
        let world = tests_support::Empty;
        let mut wire = Vec::new();
        chunk_data_packet(0x24, 0, 0, &world).write_to(&mut wire, None).unwrap();
        // Rebuild the same column minus the flag to compare lengths.
        let mut without = Vec::new();
        {
            let mut p = PacketOut::new(0x24);
            p.i32(0).i32(0);
            p.bytes(&named_root(&Nbt::Compound(vec![])));
            let mut column = Vec::new();
            for _ in 0..SECTIONS {
                let states = [0u32; ENTRIES];
                write_section(&mut column, &states);
            }
            p.var_int(column.len() as i32).bytes(&column);
            p.var_int(0);
            let light_sections = SECTIONS + 2;
            let mask = (1i64 << light_sections) - 1;
            for _ in 0..2 {
                p.var_int(1).i64(mask);
            }
            for _ in 0..2 {
                p.var_int(1).i64(0);
            }
            for _ in 0..2 {
                p.var_int(light_sections);
                for _ in 0..light_sections {
                    p.var_int(2048).bytes(&[0xFF; 2048]);
                }
            }
            p.write_to(&mut without, None).unwrap();
        }
        assert_eq!(
            wire.len(),
            without.len() + 1,
            "trustEdges must cost exactly one byte"
        );
    }

    #[test]
    fn heightmaps_use_the_named_root_form() {
        // An anonymous heightmaps root puts the client two bytes out of step
        // for the rest of the column.
        let world = tests_support::Empty;
        let mut wire = Vec::new();
        chunk_data_packet(0x21, 1, 2, &world)
            .write_to(&mut wire, None)
            .unwrap();
        let mut pos = 0usize;
        while wire[pos] & 0x80 != 0 {
            pos += 1;
        }
        pos += 1; // frame length
        pos += 1; // packet id (single byte)
        pos += 8; // x, z
        assert_eq!(wire[pos], 10, "heightmaps root tag");
        assert_eq!(&wire[pos + 1..pos + 3], &[0, 0], "empty root name length");
        assert_eq!(wire[pos + 3], 0, "empty compound closes with TAG_End");
    }
}
