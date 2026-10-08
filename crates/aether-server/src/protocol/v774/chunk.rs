//! Chunk encoding for 1.21: paletted containers and bit-packed long arrays.
//!
//! Nothing like the 1.8 layout. A section is a `short` non-air count followed
//! by two paletted containers — blocks then biomes — and the light arrives in
//! the same packet, indexed by a `BitSet` of which sections carry it.
//!
//! Values in a paletted container never straddle a `long` boundary (that
//! changed in 1.16): each `long` holds `64 / bits_per_entry` whole entries and
//! the leftover high bits are simply unused.

use aether_world::light::column::{
    compute_column, ColumnBlocks, ColumnLight, LightSection, PACKED_BYTES,
};
use aether_world::light::MAX_LIGHT;
use aether_world::BlockStateId;

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

/// Write a paletted container holding one repeated value (`bits = 0`).
///
/// No data array at all follows a single-valued container, and since 1.21.5
/// not even a length for it — see [`write_indirect`].
fn write_single_valued(out: &mut Vec<u8>, value: u32) {
    out.push(0); // bits per entry: 0 means "the whole container is this value"
    write_varint(out, value as i32);
}

/// Write an indirect paletted container.
///
/// Since 1.21.5 the long array is **not** length-prefixed: the client derives
/// the count from the bits per entry and the fixed number of entries. Sending
/// the old prefix leaves one spurious VarInt per container, and with two
/// containers per section the column desynchronises — the wreckage surfaces
/// far away as a bogus biome id, because a biome registry with one entry
/// rejects every id but zero.
fn write_indirect(out: &mut Vec<u8>, palette: &[u32], indices: &[u32]) {
    out.push(BLOCK_BITS);
    write_varint(out, palette.len() as i32);
    for v in palette {
        write_varint(out, *v as i32);
    }
    for l in &pack(indices, BLOCK_BITS) {
        out.extend_from_slice(&l.to_be_bytes());
    }
}

fn write_varint(out: &mut Vec<u8>, mut v: i32) {
    let mut u = v as u32;
    loop {
        if u & !0x7f == 0 {
            out.push(u as u8);
            return;
        }
        out.push(((u & 0x7f) | 0x80) as u8);
        u >>= 7;
        v = u as i32;
        let _ = v;
    }
}

/// Encode one section's block container, returning the non-air block count.
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
    // Biomes: one biome everywhere, so a single-valued container. Biome ids
    // are registry indices, and this server registers exactly one.
    write_single_valued(out, 0);
    non_air
}

/// Build the Chunk Data and Update Light packet (`0x2C`) for column
/// `(cx, cz)`.
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

    // Heightmaps: an empty set. The client uses them for particle and spawn
    // placement heuristics, not for rendering, and recomputes what it needs.
    p.var_int(0);

    p.var_int(column.len() as i32).bytes(&column);
    p.var_int(0); // no block entities

    write_light(&mut p, cx, cz, world);
    p
}

/// Append the four light bitsets and the sections they select.
///
/// Light is the largest part of a column by some way — `2 x sections x 2048`
/// bytes of it against roughly 8 KB of blocks — so the protocol's *empty*
/// bitsets are what matters here: a section listed in one is understood to be
/// entirely dark and its 2048 bytes are **not sent at all**. Block light is
/// zero nearly everywhere and sky light is zero under all the terrain, so most
/// of the column leaves the wire rather than being compressed away afterwards.
///
/// The four bitsets come in a fixed order — sky present, block present, sky
/// empty, block empty — and only then the payloads, each array holding one
/// entry per bit set in its *present* mask, bottom section first.
fn write_light(p: &mut PacketOut, cx: i32, cz: i32, world: &dyn BlockSource) {
    // Each bitset is written as a single 64-bit word, so the column plus its
    // two void sections has to fit in one. At 448 blocks that is 30 of 64.
    const _: () = assert!(SECTIONS as usize + 2 <= 64);

    let light = light_for_column(cx, cz, world);

    // One section below the world and one above it, neither of which the block
    // data covers. Below the floor is dark; above the sky is open.
    let dark = LightSection::Uniform(0);
    let open = LightSection::Uniform(MAX_LIGHT);
    let sky: Vec<&LightSection> = std::iter::once(&dark)
        .chain(light.sky.iter())
        .chain(std::iter::once(&open))
        .collect();
    let block: Vec<&LightSection> = std::iter::once(&dark)
        .chain(light.block.iter())
        .chain(std::iter::once(&dark))
        .collect();

    let masks = |sections: &[&LightSection]| -> (i64, i64) {
        let (mut present, mut empty) = (0i64, 0i64);
        for (i, s) in sections.iter().enumerate() {
            if s.is_dark() {
                empty |= 1 << i;
            } else {
                present |= 1 << i;
            }
        }
        (present, empty)
    };
    let (sky_present, sky_empty) = masks(&sky);
    let (block_present, block_empty) = masks(&block);

    p.var_int(1).i64(sky_present);
    p.var_int(1).i64(block_present);
    p.var_int(1).i64(sky_empty);
    p.var_int(1).i64(block_empty);

    for (sections, present) in [(&sky, sky_present), (&block, block_present)] {
        p.var_int(present.count_ones() as i32);
        for (i, s) in sections.iter().enumerate() {
            if present >> i & 1 != 0 {
                p.var_int(PACKED_BYTES as i32).bytes(&s.packed());
            }
        }
    }
}

/// Compute the light for one column from the blocks the world reports.
fn light_for_column(cx: i32, cz: i32, world: &dyn BlockSource) -> ColumnLight {
    struct View<'a> {
        cx: i32,
        cz: i32,
        world: &'a dyn BlockSource,
    }
    impl ColumnBlocks for View<'_> {
        fn sections(&self) -> usize {
            SECTIONS as usize
        }
        fn block_at(&self, x: usize, y: usize, z: usize) -> BlockStateId {
            self.world.block_at(
                self.cx * AXIS + x as i32,
                MIN_Y + y as i32,
                self.cz * AXIS + z as i32,
            )
        }
        fn props(&self, id: BlockStateId) -> aether_world::BlockProperties {
            aether_world::registry::blocks::props_of_state(id)
                .unwrap_or(aether_world::BlockProperties::AIR)
        }
    }
    compute_column(&View { cx, cz, world })
}

/// Unload column `(cx, cz)`.
pub fn unload_chunk_packet(id: i32, cx: i32, cz: i32) -> PacketOut {
    // Note the order: this packet carries Z **before** X, unlike every other
    // chunk packet in the protocol.
    let mut p = PacketOut::new(id);
    p.i32(cz).i32(cx);
    p
}

/// A stand-in world for tests in sibling modules that encode non-chunk
/// events but still have to satisfy the [`BlockSource`] parameter.
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

    /// Read back a paletted container the way a client does.
    fn read_container(b: &[u8], pos: &mut usize) -> Vec<u32> {
        let bits = b[*pos];
        *pos += 1;
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
        if bits == 0 {
            return vec![read_varint(b, pos) as u32; ENTRIES];
        }
        let palette_len = read_varint(b, pos);
        let palette: Vec<u32> = (0..palette_len)
            .map(|_| read_varint(b, pos) as u32)
            .collect();
        // Since 1.21.5 the long count is not on the wire: it follows from the
        // bits per entry and the fixed entry count.
        let per_long = 64 / bits as usize;
        let longs = ENTRIES.div_ceil(per_long);
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
        // 4 bits -> 16 whole entries per long, high bits unused.
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
        // Encode a mixed section and decode it back exactly as a client
        // would, requiring every block to land where it was generated.
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
        let got_count = i16::from_be_bytes([buf[0], buf[1]]);
        pos += 2;
        assert_eq!(got_count, count);
        let decoded = read_container(&buf, &mut pos);
        assert_eq!(decoded, states.to_vec(), "block container must round-trip");
    }

    #[test]
    fn uniform_section_uses_the_single_valued_form() {
        // An all-air section must not pay for a palette or a data array.
        let states = [0u32; ENTRIES];
        let mut buf = Vec::new();
        assert_eq!(write_section(&mut buf, &states), 0, "no non-air blocks");
        assert_eq!(buf[2], 0, "bits per entry must be 0");
    }

    /// An ocean column: stone floor, water up to sea level, air above —
    /// the shape the generator actually produces around spawn.
    struct Ocean;
    impl BlockSource for Ocean {
        fn block_at(&self, _x: i32, y: i32, _z: i32) -> BlockStateId {
            match y {
                0 => b::BEDROCK,
                1..=39 => b::STONE,
                40 => b::SAND,
                41..=63 => b::WATER,
                _ => b::AIR,
            }
        }
    }

    #[test]
    fn compression_collapses_a_column_to_a_fraction_of_its_wire_size() {
        // A column is mostly runs of identical bytes — 0xFF light for every
        // section, and long stretches of one block state — so it deflates
        // enormously. This is the number the join burst is made of: at a view
        // radius of 8 the client is sent (2*8+1)^2 = 289 of these.
        let pkt = chunk_data_packet(0x2C, 0, 0, &Ocean);

        let mut plain = Vec::new();
        pkt.write_to(&mut plain, None).unwrap();
        let mut deflated = Vec::new();
        pkt.write_to(&mut deflated, Some(256)).unwrap();

        println!(
            "column: {} B uncompressed -> {} B compressed ({:.1}x); \
             289-column burst {:.1} MiB -> {:.1} MiB",
            plain.len(),
            deflated.len(),
            plain.len() as f64 / deflated.len() as f64,
            (plain.len() * 289) as f64 / (1024.0 * 1024.0),
            (deflated.len() * 289) as f64 / (1024.0 * 1024.0),
        );

        assert!(
            deflated.len() * 10 < plain.len(),
            "expected better than a tenth: {} -> {}",
            plain.len(),
            deflated.len()
        );
    }

    #[test]
    fn water_reaches_the_client_as_water_at_every_level_below_the_sea() {
        // The user reports a 1.21 client not registering that it is submerged.
        // Before blaming the client, prove the wire carries water: decode the
        // whole column the way a client parses it and check the state at each
        // y, rather than trusting the encoder's own view of what it wrote.
        let mut wire = Vec::new();
        chunk_data_packet(0x2C, 0, 0, &Ocean)
            .write_to(&mut wire, None)
            .unwrap();

        // Skip the frame length varint, the packet id, x, z, and the (empty)
        // heightmap array, then take the chunk-data byte array.
        let mut pos = 0usize;
        let varint = |b: &[u8], pos: &mut usize| -> i32 {
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
        varint(&wire, &mut pos); // frame length
        varint(&wire, &mut pos); // packet id
        pos += 8; // x, z
        assert_eq!(varint(&wire, &mut pos), 0, "no heightmaps are sent");
        let data_len = varint(&wire, &mut pos) as usize;
        let data = &wire[pos..pos + data_len];

        let water = block_state(b::WATER);
        let mut p = 0usize;
        for sy in 0..SECTIONS {
            p += 2; // block count
            let blocks = read_container(data, &mut p);
            let biomes = read_container(data, &mut p);
            assert_eq!(biomes[0], 0, "the one registered biome is index 0");
            for ly in 0..AXIS {
                let wy = MIN_Y + sy * AXIS + ly;
                let got = blocks[(ly * 256) as usize]; // x=0, z=0
                let want = block_state(Ocean.block_at(0, wy, 0));
                assert_eq!(got, want, "block state at y={wy}");
                if (41..=63).contains(&wy) {
                    assert_eq!(got, water, "y={wy} must be water, not {got}");
                }
            }
        }
        assert_eq!(p, data.len(), "the column must be consumed exactly");
    }

    #[test]
    fn unload_packet_puts_z_before_x() {
        let mut wire = Vec::new();
        unload_chunk_packet(0x25, 3, -5)
            .write_to(&mut wire, None)
            .unwrap();
        // frame len, packet id, then z, then x
        let body = &wire[2..];
        assert_eq!(i32::from_be_bytes(body[0..4].try_into().unwrap()), -5);
        assert_eq!(i32::from_be_bytes(body[4..8].try_into().unwrap()), 3);
    }
}
