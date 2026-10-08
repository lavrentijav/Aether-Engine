//! Chunk encoding for 1.18: paletted containers for blocks *and* biomes.
//!
//! 1.18 is the release that moved biomes into the section itself. Before it a
//! column carried a separate flat biome array and a bitmask saying which
//! sections were present; here every section in the column is always written,
//! each as a `short` non-air count followed by two paletted containers —
//! blocks, then biomes at 4×4×4 resolution.
//!
//! Two things differ from the otherwise identical 1.21 encoding, and both are
//! easy to get wrong by copying the newer codec:
//!
//! * heightmaps are **named**-root NBT here, not the anonymous form;
//! * Unload Chunk carries **X then Z**, the order 1.21 later reversed.

use super::super::BlockSource;
use super::block_state;
use super::registry::{named_root, HEIGHT, MIN_Y, SECTIONS};
use crate::proto::PacketOut;

/// Blocks per section along each axis.
const AXIS: i32 = 16;
/// Entries in one section's block container: `16³`.
const ENTRIES: usize = 4096;
/// Bits per block palette index. Ten engine blocks fit the format's 4-bit
/// minimum for block containers with room to spare.
const BLOCK_BITS: u8 = 4;
/// Bits per heightmap entry: enough to hold `0..=HEIGHT`.
///
/// Derived, not written down. It used to be a literal 8, which was right only
/// while the world was 128 blocks tall — the moment the column grew to 384 the
/// heightmap silently truncated every value above 255, and the client would
/// have placed particles and mob spawns against a ceiling that is not there.
const HEIGHTMAP_BITS: u8 = {
    let mut bits = 1u8;
    while (1i64 << bits) <= HEIGHT as i64 {
        bits += 1;
    }
    bits
};

/// Pack palette indices into the protocol's long array.
///
/// Since 1.16 a value never straddles a `long`: each holds
/// `64 / bits` whole entries and the leftover high bits stay unused.
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

/// A container holding one repeated value: `bits = 0`, no data array.
fn write_single_valued(out: &mut Vec<u8>, value: u32) {
    out.push(0);
    write_varint(out, value as i32);
    write_varint(out, 0);
}

/// An indirect container: palette, then packed indices.
fn write_indirect(out: &mut Vec<u8>, palette: &[u32], indices: &[u32], bits: u8) {
    out.push(bits);
    write_varint(out, palette.len() as i32);
    for v in palette {
        write_varint(out, *v as i32);
    }
    let longs = pack(indices, bits);
    write_varint(out, longs.len() as i32);
    for l in &longs {
        out.extend_from_slice(&l.to_be_bytes());
    }
}

/// Encode one section: non-air count, block container, biome container.
fn write_section(out: &mut Vec<u8>, states: &[u32; ENTRIES]) -> i16 {
    let non_air = states.iter().filter(|s| **s != 0).count() as i16;

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
        write_indirect(out, &palette, &indices, BLOCK_BITS);
    }
    // Biomes: this server registers exactly one, so every section is a
    // single-valued container pointing at registry id 0.
    write_single_valued(out, 0);
    non_air
}

/// `MOTION_BLOCKING`: for each of the 256 columns, the height of the highest
/// non-air block as `y - min_y + 1`, or 0 for an empty column.
///
/// The client uses this for particle and placement heuristics rather than
/// rendering, but an absent or malformed heightmap is a needless way to have
/// it disagree with the world it was just sent.
fn motion_blocking(cx: i32, cz: i32, world: &dyn BlockSource) -> Vec<i64> {
    let mut values = Vec::with_capacity(256);
    for z in 0..AXIS {
        for x in 0..AXIS {
            let mut h = 0u32;
            for y in (MIN_Y..MIN_Y + HEIGHT).rev() {
                if world.block_at(cx * AXIS + x, y, cz * AXIS + z) != aether_world::BlockStateId::AIR
                {
                    h = (y - MIN_Y + 1) as u32;
                    break;
                }
            }
            values.push(h);
        }
    }
    pack(&values, HEIGHTMAP_BITS)
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

    // Heightmaps, as a *named* root compound.
    let heightmaps = super::super::nbt::Nbt::Compound(vec![(
        "MOTION_BLOCKING".into(),
        super::super::nbt::Nbt::LongArray(motion_blocking(cx, cz, world)),
    )]);
    p.bytes(&named_root(&heightmaps));

    p.var_int(column.len() as i32).bytes(&column);
    p.var_int(0); // no block entities
    p.bool(true); // trust edges

    // Full-bright: every section, plus the void section below and above the
    // column, carries maximum sky and block light.
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
    // X before Z in this generation; 1.21 swapped them.
    let mut p = PacketOut::new(id);
    p.i32(cx).i32(cz);
    p
}

/// A stand-in world for sibling tests that encode non-chunk events but still
/// have to satisfy the [`BlockSource`] parameter.
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

    /// Entries in one section's biome container: `4³`, at 4×4×4 resolution.
    /// Only the decoding side needs the count — the encoder writes every
    /// biome container in the single-valued form, which carries no data array.
    const BIOME_ENTRIES: usize = 64;

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

    /// Read back a paletted container the way a client does.
    fn read_container(b: &[u8], pos: &mut usize, entries: usize) -> Vec<u32> {
        let bits = b[*pos];
        *pos += 1;
        if bits == 0 {
            let value = read_varint(b, pos) as u32;
            assert_eq!(read_varint(b, pos), 0, "single-valued carries no data");
            return vec![value; entries];
        }
        let palette_len = read_varint(b, pos);
        let palette: Vec<u32> = (0..palette_len)
            .map(|_| read_varint(b, pos) as u32)
            .collect();
        let longs = read_varint(b, pos) as usize;
        let per_long = 64 / bits as usize;
        let mut out = Vec::with_capacity(entries);
        for _ in 0..longs {
            let l = u64::from_be_bytes(b[*pos..*pos + 8].try_into().unwrap());
            *pos += 8;
            for i in 0..per_long {
                if out.len() == entries {
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
    fn section_carries_a_biome_container_after_the_blocks() {
        // The 1.18 change: biomes live in the section. Decode blocks, then
        // require a second container to follow and to name registry id 0.
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

        let mut pos = 0usize;
        assert_eq!(i16::from_be_bytes([buf[0], buf[1]]), count);
        pos += 2;
        let blocks = read_container(&buf, &mut pos, ENTRIES);
        assert_eq!(blocks, states.to_vec(), "block container must round-trip");
        let biomes = read_container(&buf, &mut pos, BIOME_ENTRIES);
        assert_eq!(biomes, vec![0u32; BIOME_ENTRIES], "one registered biome");
        assert_eq!(pos, buf.len(), "nothing trails the two containers");
    }

    #[test]
    fn uniform_section_uses_the_single_valued_form() {
        let states = [0u32; ENTRIES];
        let mut buf = Vec::new();
        assert_eq!(write_section(&mut buf, &states), 0, "no non-air blocks");
        assert_eq!(buf[2], 0, "bits per entry must be 0");
    }

    #[test]
    fn heightmap_reports_the_top_solid_block() {
        // The flat test world tops out at grass on y=31; a heightmap entry is
        // measured from the bottom of the column, so the value is that height
        // above MIN_Y rather than an absolute Y.
        let longs = motion_blocking(0, 0, &Flat);
        // Entries never straddle a long, so the count rounds *up*: at 9 bits
        // that is seven per long and 37 longs, not 36.
        let per_long = 64 / HEIGHTMAP_BITS as usize;
        assert_eq!(longs.len(), 256usize.div_ceil(per_long));
        for (n, l) in longs.iter().enumerate() {
            for i in 0..per_long {
                if n * per_long + i >= 256 {
                    continue; // padding in the last long
                }
                let mask = (1u64 << HEIGHTMAP_BITS) - 1;
                let v = ((*l as u64) >> (i * HEIGHTMAP_BITS as usize)) & mask;
                assert_eq!(
                    v as i32,
                    31 - MIN_Y + 1,
                    "column {} height",
                    n * per_long + i
                );
            }
        }
    }

    #[test]
    fn heightmap_is_zero_for_an_empty_column() {
        let longs = motion_blocking(0, 0, &tests_support::Empty);
        assert!(longs.iter().all(|l| *l == 0), "air column has height 0");
    }

    #[test]
    fn column_round_trips_every_section_through_the_client_view() {
        // The whole packet, decoded as a client does: all sections present in
        // order, every block back where the world put it.
        let mut wire = Vec::new();
        chunk_data_packet(0x22, 3, -5, &Flat).write_to(&mut wire, None).unwrap();

        let mut pos = 0usize;
        let _frame = read_varint(&wire, &mut pos);
        assert_eq!(read_varint(&wire, &mut pos), 0x22);
        assert_eq!(
            i32::from_be_bytes(wire[pos..pos + 4].try_into().unwrap()),
            3
        );
        pos += 4;
        assert_eq!(
            i32::from_be_bytes(wire[pos..pos + 4].try_into().unwrap()),
            -5
        );
        pos += 4;

        // Heightmaps: named root compound (tag, empty name, one LongArray).
        assert_eq!(wire[pos], 10, "root TAG_Compound");
        assert_eq!(&wire[pos + 1..pos + 3], &[0, 0], "empty root name");
        pos += 3;
        assert_eq!(wire[pos], 12, "TAG_Long_Array");
        let name_len = u16::from_be_bytes([wire[pos + 1], wire[pos + 2]]) as usize;
        assert_eq!(&wire[pos + 3..pos + 3 + name_len], b"MOTION_BLOCKING");
        pos += 3 + name_len;
        let n = i32::from_be_bytes(wire[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4 + n * 8;
        assert_eq!(wire[pos], 0, "TAG_End closes the compound");
        pos += 1;

        let data_len = read_varint(&wire, &mut pos) as usize;
        let data_end = pos + data_len;
        for sy in 0..SECTIONS {
            let count = i16::from_be_bytes([wire[pos], wire[pos + 1]]);
            pos += 2;
            let blocks = read_container(&wire, &mut pos, ENTRIES);
            let _ = read_container(&wire, &mut pos, BIOME_ENTRIES);
            let expected_non_air = blocks.iter().filter(|s| **s != 0).count() as i16;
            assert_eq!(count, expected_non_air, "section {sy} non-air count");
            for (i, got) in blocks.iter().enumerate() {
                let x = (i & 15) as i32;
                let z = ((i >> 4) & 15) as i32;
                let y = ((i >> 8) & 15) as i32;
                let wy = MIN_Y + sy * AXIS + y;
                let want = block_state(Flat.block_at(3 * AXIS + x, wy, -5 * AXIS + z));
                assert_eq!(*got, want, "block at section {sy} index {i}");
            }
        }
        assert_eq!(pos, data_end, "sections must fill the declared buffer");
    }

    #[test]
    fn unload_packet_puts_x_before_z() {
        // 1.21 reversed this; copying the newer codec would swap the column.
        let mut wire = Vec::new();
        unload_chunk_packet(0x1D, 3, -5).write_to(&mut wire, None).unwrap();
        let body = &wire[2..];
        assert_eq!(i32::from_be_bytes(body[0..4].try_into().unwrap()), 3);
        assert_eq!(i32::from_be_bytes(body[4..8].try_into().unwrap()), -5);
    }
}
