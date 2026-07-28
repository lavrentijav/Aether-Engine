//! Build a Minecraft 1.8.9 "Chunk Data" (0x21) packet from the engine world.
//!
//! 1.8 column layout, ground-up continuous: for each present 16³ section, an
//! array of `4096` little-endian `u16` block values `(id << 4) | meta`, then a
//! 2048-byte block-light nibble array, then a 2048-byte sky-light nibble array;
//! after all sections, a 256-byte biome map. Light is filled to **0xFF**
//! (full-bright) so the world is always maximally lit.

use crate::proto::PacketOut;
use aether_world::light::MAX_LIGHT;
use aether_world::BlockStateId;

/// Number of 16-block-tall sections in a 1.8 column.
const SECTIONS: usize = 16;

/// Map an engine block id to a vanilla 1.8 numeric block id.
fn vanilla_id(id: BlockStateId) -> u16 {
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
    let mut data: Vec<u8> = Vec::new();
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
        for v in blocks {
            data.extend_from_slice(&v.to_le_bytes());
        }
        data.extend_from_slice(&full_light()); // block light
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
        pkt.send(&mut wire).unwrap();
        // 8192 blocks + 2048 + 2048 + 256 biomes + headers ≈ 12.5 KiB.
        assert!(wire.len() > 12_000, "unexpected chunk size {}", wire.len());
    }
}
