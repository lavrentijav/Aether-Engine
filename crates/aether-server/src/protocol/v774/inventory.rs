//! The player's hotbar as 1.21.11 wants it on the wire.
//!
//! Items are not block states: `minecraft:dirt` is item 28 but block state 10,
//! and the two numbering spaces are unrelated. Ids here come from
//! `minecraft-data` `pc/1.21.11/items.json`.
//!
//! Since 1.20.5 an item stack is a count followed by an id and **two component
//! counts** — the added and removed data components — rather than the old
//! id/count/damage/NBT triple. A plain block needs no components, so both
//! counts are zero, but they are not optional: omitting them shifts every
//! following slot.

use aether_api::block_ids as b;
use aether_world::BlockStateId;

use crate::proto::PacketOut;


/// What every player is handed to build with, as `(engine block, item id)`.
///
/// Kept to blocks the engine can actually store, so anything placed from the
/// hotbar round-trips into the world rather than being silently refused.
pub const HOTBAR: [(BlockStateId, i32); 7] = [
    (b::STONE, 1),
    (b::DIRT, 28),
    (b::GRASS_BLOCK, 27),
    (b::SAND, 59),
    (b::GRAVEL, 63),
    (b::OAK_LOG, 134),
    (b::OAK_LEAVES, 182),
];

// The item→block lookup that used to live here searched `HOTBAR`, seven
// entries long, and everything it missed was placed as stone. It is gone:
// `Codec::block_for_item` now resolves any name through the block registry,
// which knows all 1166 of them.

/// The engine block a hotbar slot places, if that slot holds one.
///
/// `slot` is a hotbar index (`0..9`), the form the client reports when it
/// changes selection.
pub fn block_in_hotbar_slot(slot: usize) -> Option<BlockStateId> {
    HOTBAR.get(slot).map(|(block, _)| *block)
}

/// Append an empty stack: a zero count and nothing else.
pub(super) fn write_empty(p: &mut PacketOut) {
    p.var_int(0);
}

/// Append one stack of `item_id`.
pub(super) fn write_item(p: &mut PacketOut, item_id: i32, count: i32) {
    p.var_int(count)
        .var_int(item_id)
        .var_int(0) // no components added to the default set
        .var_int(0); // and none removed
}

/// Data component id of `minecraft:custom_name`.
///
/// From `minecraft-data` `pc/1.21.11/protocol.json`, cross-checked against the
/// `--reports` dump of the vanilla server's `data_component_type` registry —
/// two independent sources, because a wrong component id is read as a
/// different component entirely and the client rejects the whole stack.
const COMPONENT_CUSTOM_NAME: i32 = 6;

/// Append one stack of `item_id` carrying a display name.
///
/// The name is how a server-driven window says anything at all: a slot has no
/// other text, and the stack-count badge tops out at what fits in a corner of
/// a 16-pixel icon. So a quantity that will not read as a badge is put in the
/// name instead — see [`super::write_container_slot`].
pub(super) fn write_named_item(p: &mut PacketOut, item_id: i32, count: i32, name: &str) {
    p.var_int(count)
        .var_int(item_id)
        .var_int(1) // one component added...
        .var_int(0) // ...and none removed
        .var_int(COMPONENT_CUSTOM_NAME);
    // An *anonymous* NBT component, the same shape System Chat uses here.
    p.bytes(&crate::protocol::nbt::string(name).to_network());
}

/// Set Held Item: which hotbar slot the client should have selected.
pub fn held_item_packet(id: i32, slot: i32) -> PacketOut {
    let mut p = PacketOut::new(id);
    p.var_int(slot);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotbar_slots_map_back_to_engine_blocks() {
        assert_eq!(block_in_hotbar_slot(0), Some(b::STONE));
        assert_eq!(block_in_hotbar_slot(6), Some(b::OAK_LEAVES));
        assert_eq!(block_in_hotbar_slot(7), None, "slot past the table is empty");
    }
}
