//! End-to-end anti-xray: a buried ore must not reach the wire.
//!
//! This drives the real 1.21.11 codec over a column, not just the graph. The
//! strong claim is the byte-equality one: encoding a world with a buried ore
//! through [`EngineColumn`] must produce exactly the bytes of the same world
//! with the ore replaced by the rock around it — i.e. the client literally
//! receives the rock, palette and light included.

use aether_server::protocol::{codec_for, BlockSource, ServerEvent};
use aether_server::visibility::{build_hidden_states, EngineColumn};
use aether_world::registry::blocks;
use aether_world::BlockStateId;

/// A deep, otherwise uniform deepslate column with one ore at `ore_at`.
struct World {
    deepslate: BlockStateId,
    ore: BlockStateId,
    /// `None` means the cell is deepslate like the rest.
    ore_at: Option<(i32, i32, i32)>,
}

impl World {
    fn new(ore_at: Option<(i32, i32, i32)>, ore: BlockStateId) -> Self {
        Self {
            deepslate: blocks::default_state("minecraft:deepslate").unwrap(),
            ore,
            ore_at,
        }
    }
}

impl BlockSource for World {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        if Some((x, y, z)) == self.ore_at {
            self.ore
        } else {
            self.deepslate
        }
    }
}

fn encode(source: &dyn BlockSource, cx: i32, cz: i32) -> Vec<u8> {
    let codec = codec_for(774).expect("1.21.11 codec");
    let pkts = codec.encode(&ServerEvent::ChunkColumn { cx, cz }, source);
    let mut wire = Vec::new();
    for p in &pkts {
        wire.extend_from_slice(&p.frame(None));
    }
    wire
}

#[test]
fn a_buried_ore_is_indistinguishable_from_the_rock_around_it() {
    let ore = blocks::default_state("minecraft:diamond_ore").unwrap();
    let at = (8, -20, 8);

    let with_ore = World::new(Some(at), ore);
    let without_ore = World::new(None, ore);

    let hidden = build_hidden_states(&["minecraft:diamond_ore".to_string()]);
    let masked = EngineColumn::new(&with_ore, &hidden);

    let plain = encode(&with_ore, 0, 0);
    let real = encode(&masked, 0, 0);
    let rock = encode(&without_ore, 0, 0);
    assert!(
        masked.masked_any(),
        "the codec read the buried ore and hid it"
    );

    assert_ne!(
        plain, real,
        "the unmasked column must differ from the masked one"
    );
    assert_eq!(
        real, rock,
        "the masked column must be byte-identical to the rock-only world"
    );
}

#[test]
fn an_exposed_ore_reaches_the_client() {
    // The ore touches open air (every cell that is not the ore is air above
    // it), so it is not buried and must be sent as itself.
    struct Exposed {
        air: BlockStateId,
        ore: BlockStateId,
    }
    impl BlockSource for Exposed {
        fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
            if (x, y, z) == (8, -20, 8) {
                self.ore
            } else {
                self.air
            }
        }
    }
    let world = Exposed {
        air: BlockStateId::AIR,
        ore: blocks::default_state("minecraft:diamond_ore").unwrap(),
    };
    let hidden = build_hidden_states(&["minecraft:diamond_ore".to_string()]);
    let masked = EngineColumn::new(&world, &hidden);
    let plain = encode(&world, 0, 0);
    let real = encode(&masked, 0, 0);
    assert_eq!(
        plain, real,
        "an exposed ore must be sent unchanged (nothing to hide)"
    );
}
