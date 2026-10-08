//! Build a Minecraft 1.8.9 "Chunk Data" (0x21) packet from the engine world.
//!
//! 1.8 column layout, ground-up continuous. The arrays are grouped **by kind**
//! across the whole column, not interleaved per section: first every present
//! section's array of `4096` little-endian `u16` block values `(id << 4) | meta`,
//! then every section's 2048-byte block-light nibble array, then every section's
//! 2048-byte sky-light nibble array, then a 256-byte biome map. Light is filled
//! to **0xFF** (full-bright) so the world is always maximally lit.

use crate::proto::PacketOut;
use aether_world::light::MAX_LIGHT;
use aether_world::BlockStateId;

/// Number of 16-block-tall sections in a 1.8 column.
pub(super) const SECTIONS: usize = 16;

/// Map an engine block id to a vanilla 1.8 numeric block id.
pub fn vanilla_id(id: BlockStateId) -> u16 {
    use aether_api::block_ids as b;
    if id == b::AIR {
        0
    } else if id == b::STONE {
        1
    } else if id == b::GRASS_BLOCK {
        2
    } else if id == b::DIRT {
        3
    } else if id == b::BEDROCK {
        7
    } else if id == b::WATER {
        9
    } else if id == b::SAND {
        12
    } else if id == b::GRAVEL {
        13
    } else if id == b::OAK_LOG {
        17
    } else if id == b::OAK_LEAVES {
        18
    } else {
        1 // unknown -> stone, so the client always has something solid
    }
}

/// Map a vanilla 1.8 numeric block id back to an engine block id — the inverse
/// of [`vanilla_id`], used to turn a block the client says it placed into
/// something the engine world can store. `None` for ids the engine has no
/// block for, so placement of an unknown item is refused rather than guessed.
pub fn engine_id(vanilla: u16) -> Option<BlockStateId> {
    use aether_api::block_ids as b;
    Some(match vanilla {
        0 => b::AIR,
        1 => b::STONE,
        2 => b::GRASS_BLOCK,
        3 => b::DIRT,
        7 => b::BEDROCK,
        8 | 9 => b::WATER,
        12 => b::SAND,
        13 => b::GRAVEL,
        17 => b::OAK_LOG,
        18 => b::OAK_LEAVES,
        _ => return None,
    })
}

/// A full-nibble (all `0xFF`) light array: 2048 bytes = 4096 nibbles.
fn full_light() -> [u8; 2048] {
    debug_assert_eq!(MAX_LIGHT, 15);
    [0xFF; 2048]
}

/// Build the Chunk Data packet for column `(cx, cz)`.
///
/// `get` returns the engine block id at absolute `(x, y, z)`.
pub fn chunk_data_packet(
    cx: i32,
    cz: i32,
    get: &dyn Fn(i32, i32, i32) -> BlockStateId,
) -> PacketOut {
    let mut sections: Vec<[u16; 4096]> = Vec::new();
    let mut bitmask: u16 = 0;

    for sy in 0..SECTIONS {
        // Decode this section's blocks in YZX order.
        let mut blocks = [0u16; 4096];
        let mut any = false;
        for (i, slot) in blocks.iter_mut().enumerate() {
            let x = (i & 15) as i32;
            let z = ((i >> 4) & 15) as i32;
            let y = ((i >> 8) & 15) as i32;
            let wy = sy as i32 * 16 + y;
            let vid = vanilla_id(get(cx * 16 + x, wy, cz * 16 + z));
            if vid != 0 {
                any = true;
            }
            *slot = vid << 4; // meta = 0
        }
        if !any {
            continue; // empty section -> not sent
        }
        bitmask |= 1 << sy;
        sections.push(blocks);
    }

    // 1.8 groups the column's arrays *by kind*, not by section: every present
    // section's block array first, then every section's block-light array,
    // then every section's sky-light array, then the biome map. Interleaving
    // them per section keeps the total length right but shifts each section's
    // blocks into the section above (and feeds light bytes back as block ids),
    // which is what put whole sub-chunks of water in the sky.
    let mut data: Vec<u8> = Vec::with_capacity(sections.len() * (8192 + 2048 + 2048) + 256);
    for blocks in &sections {
        for v in blocks {
            data.extend_from_slice(&v.to_le_bytes());
        }
    }
    for _ in &sections {
        data.extend_from_slice(&full_light()); // block light
    }
    for _ in &sections {
        data.extend_from_slice(&full_light()); // sky light (overworld)
    }

    // Biome map: 256 bytes, "plains" (1).
    data.extend_from_slice(&[1u8; 256]);

    let mut p = PacketOut::new(0x21);
    p.i32(cx)
        .i32(cz)
        .bool(true) // ground-up continuous
        .u16(bitmask)
        .var_int(data.len() as i32)
        .bytes(&data);
    p
}

/// Unload column `(cx, cz)`: a ground-up-continuous Chunk Data packet with an
/// empty section bitmask and no data, which a vanilla 1.8 client interprets
/// as "discard this column" rather than "empty air column".
pub fn unload_chunk_packet(cx: i32, cz: i32) -> PacketOut {
    let mut p = PacketOut::new(0x21);
    p.i32(cx).i32(cz).bool(true).u16(0).var_int(0);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::block_ids as b;

    #[test]
    fn vanilla_mapping_covers_flat_blocks() {
        assert_eq!(vanilla_id(b::AIR), 0);
        assert_eq!(vanilla_id(b::BEDROCK), 7);
        assert_eq!(vanilla_id(b::DIRT), 3);
        assert_eq!(vanilla_id(b::GRASS_BLOCK), 2);
    }

    /// Decode a framed 0x21 packet the way a vanilla 1.8 client does and
    /// return `(bitmask, blocks-per-present-section)`.
    fn decode_chunk_packet(wire: &[u8]) -> (u16, Vec<(usize, Vec<u16>)>) {
        let mut pos = 0usize;
        let read_varint = |pos: &mut usize| {
            let mut v = 0i32;
            for i in 0..5 {
                let byte = wire[*pos];
                *pos += 1;
                v |= ((byte & 0x7f) as i32) << (7 * i);
                if byte & 0x80 == 0 {
                    break;
                }
            }
            v
        };
        let _len = read_varint(&mut pos);
        assert_eq!(read_varint(&mut pos), 0x21);
        pos += 4 + 4; // cx, cz
        assert_eq!(wire[pos], 1, "ground-up continuous");
        pos += 1;
        let bitmask = u16::from_be_bytes([wire[pos], wire[pos + 1]]);
        pos += 2;
        let size = read_varint(&mut pos) as usize;
        let data = &wire[pos..pos + size];

        let present: Vec<usize> = (0..SECTIONS).filter(|s| bitmask & (1 << s) != 0).collect();
        assert_eq!(
            data.len(),
            present.len() * (8192 + 2048 + 2048) + 256,
            "column payload size"
        );
        let mut out = Vec::new();
        for (n, &sy) in present.iter().enumerate() {
            let base = n * 8192;
            let blocks = (0..4096)
                .map(|i| u16::from_le_bytes([data[base + i * 2], data[base + i * 2 + 1]]))
                .collect();
            out.push((sy, blocks));
        }
        (bitmask, out)
    }

    #[test]
    fn column_arrays_are_grouped_by_kind_not_per_section() {
        // Regression: the payload used to be written as
        // [blocks|blocklight|skylight] per section. A 1.8 client reads all the
        // block arrays back to back, so every section after the first was fed
        // the previous section's light bytes as block ids — terrain slid
        // upwards and whole sub-chunks of below-sea-level water reappeared in
        // the sky. Decode the packet as the client does and require every
        // block to land back where it was generated.
        let get = |x: i32, y: i32, z: i32| {
            if y == 0 {
                b::BEDROCK
            } else if y < 40 {
                b::STONE
            } else if y <= 63 {
                // Below sea level: water, the block that showed up in the sky.
                b::WATER
            } else if y == 64 && (x + z) % 2 == 0 {
                b::GRASS_BLOCK
            } else {
                b::AIR
            }
        };
        let mut wire = Vec::new();
        chunk_data_packet(3, -5, &get).write_to(&mut wire, None).unwrap();
        let (bitmask, sections) = decode_chunk_packet(&wire);
        // Sections 0..=4 hold terrain; nothing above may be sent.
        assert_eq!(bitmask, 0b0000_0000_0001_1111, "section bitmask");

        for (sy, blocks) in &sections {
            for (i, &raw) in blocks.iter().enumerate() {
                let x = (i & 15) as i32;
                let z = ((i >> 4) & 15) as i32;
                let y = ((i >> 8) & 15) as i32;
                let wy = *sy as i32 * 16 + y;
                let want = vanilla_id(get(3 * 16 + x, wy, -5 * 16 + z));
                assert_eq!(
                    raw >> 4,
                    want,
                    "block at ({x}, {wy}, {z}) decoded as {} not {want}",
                    raw >> 4
                );
                assert_eq!(raw & 0xF, 0, "meta must be zero");
            }
        }
        // And specifically: no water anywhere above sea level.
        for (sy, blocks) in &sections {
            for (i, &raw) in blocks.iter().enumerate() {
                let wy = *sy as i32 * 16 + ((i >> 8) & 15) as i32;
                assert!(
                    !(raw >> 4 == 9 && wy > 63),
                    "water at y={wy}, above sea level"
                );
            }
        }
    }

    #[test]
    fn flat_column_has_one_section() {
        // A flat column: bedrock/dirt/grass in section 0, air above.
        let get = |_x: i32, y: i32, _z: i32| {
            if y == 0 {
                b::BEDROCK
            } else if (1..=2).contains(&y) {
                b::DIRT
            } else if y == 3 {
                b::GRASS_BLOCK
            } else {
                b::AIR
            }
        };
        let pkt = chunk_data_packet(0, 0, &get);
        // Payload should be substantial (one section + biomes) and non-empty.
        let mut wire = Vec::new();
        pkt.write_to(&mut wire, None).unwrap();
        // 8192 blocks + 2048 + 2048 + 256 biomes + headers ≈ 12.5 KiB.
        assert!(wire.len() > 12_000, "unexpected chunk size {}", wire.len());
    }
}
