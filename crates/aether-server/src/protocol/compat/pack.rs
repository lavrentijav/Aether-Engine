//! The resource-pack half of the compat layer (task RP1, Part B).
//!
//! A block the client lacks is shown as a compat stand-in (J2). The stand-in
//! is the right *shape* but the wrong *picture*: pale-oak planks look like oak.
//! A resource pack can draw the real picture on a block the client *does*
//! have, but only if that block is not itself used visibly — otherwise real oak
//! planks and disguised pale-oak planks would share a texture.
//!
//! The trick is a **carrier block with spare states**: `note_block` (23
//! instruments × 25 notes × powered) has hundreds of states, a player almost
//! never places all of them, and the pack's `blockstates` JSON can draw a
//! different model per state. The server places one carrier state for each
//! disguised block; genuine carriers stay on a canonical state the pack leaves
//! vanilla. See `docs/design/compat-resource-pack.md`.
//!
//! # Why the carrier is an *engine* state
//!
//! A [`PackTable`] maps an engine block state to the engine state of the
//! carrier to show instead. That engine state then flows through the codec's
//! normal per-version block translation ([`super::blocks`]) exactly as any
//! other engine block would — so a carrier chosen with properties the client's
//! release shares (a note and instrument it has) lands on the matching state
//! there, and no codec needs to know this module exists. The remap is applied
//! per player at render time ([`CarrierView`]), only when that player has
//! successfully loaded the pack.
//!
//! A carrier is a *disguise*: the world still holds the real block, every
//! action still refers to it, and the server's own physics, light and
//! collision are unaffected. Only what the client is sent changes.
//!
//! # No pack, no carrier
//!
//! A player who has not loaded the pack gets the plain stand-in. The status is
//! tracked per player; a client that declines (or a release with no status
//! handshake) keeps the stand-in rather than seeing a raw note block.

use aether_world::BlockStateId;

use super::pack_gen;

/// One release's carrier assignment: engine block state → the engine state of
/// the carrier to show in its place, sorted by the first so lookup is a binary
/// search.
#[derive(Clone, Copy)]
pub struct PackTable {
    /// `(engine block state, carrier engine block state)`, sorted by the first.
    pub carriers: &'static [(u32, u32)],
    /// The resource-pack `pack_format` this assignment was generated for. The
    /// pack offered to a client of this release must declare the same format.
    pub pack_format: u32,
}

impl PackTable {
    /// The carrier engine state to show for `id`, or `None` when `id` is not
    /// disguised and should render as itself.
    #[inline]
    pub fn carrier(&self, id: BlockStateId) -> Option<BlockStateId> {
        let raw = id.raw();
        let i = self.carriers.binary_search_by_key(&raw, |e| e.0).ok()?;
        Some(BlockStateId(self.carriers[i].1))
    }

    /// Whether this release has any carrier assignments at all.
    pub fn is_empty(&self) -> bool {
        self.carriers.is_empty()
    }
}

/// The carrier table for a codec's protocol id, if that release has a
/// generated pack. `None` when there is no table, or the table is empty.
pub fn table_for(protocol: i32) -> Option<&'static PackTable> {
    let t = match protocol {
        763 => &V1_20_1,
        _ => return None,
    };
    (!t.is_empty()).then_some(t)
}

macro_rules! pack_table {
    ($name:ident, $m:ident) => {
        /// The carrier table for this release.
        pub static $name: PackTable = PackTable {
            carriers: pack_gen::$m::CARRIERS,
            pack_format: pack_gen::$m::PACK_FORMAT,
        };
    };
}

pack_table!(V1_20_1, v1_20_1); // 1.20.1, protocol 763

/// A [`super::super::BlockSource`] that swaps each engine block state for the
/// carrier state the pack draws the real block on.
///
/// Applied outermost, so it sees exactly the states the codec is about to
/// translate: a state the world holds is replaced before the per-version block
/// table ever looks at it. A masked column (anti-xray) hides its sections as
/// stone, which is not disguised, so the two wrappers do not interact.
pub struct CarrierView<'a> {
    inner: &'a dyn super::super::BlockSource,
    table: &'static PackTable,
}

impl<'a> CarrierView<'a> {
    /// Wrap `inner`, remapping through `table`.
    pub fn new(inner: &'a dyn super::super::BlockSource, table: &'static PackTable) -> Self {
        Self { inner, table }
    }
}

impl super::super::BlockSource for CarrierView<'_> {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        let block = self.inner.block_at(x, y, z);
        self.table.carrier(block).unwrap_or(block)
    }

    fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        self.inner.copy_section(cx, cy, cz, out);
        for slot in out.iter_mut() {
            if let Some(carrier) = self.table.carrier(*slot) {
                *slot = carrier;
            }
        }
    }

    fn column_biomes(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<std::sync::Arc<aether_worldgen::ColumnBiomes>> {
        self.inner.column_biomes(cx, cz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_world::registry::blocks::default_state;

    #[test]
    fn a_protocol_without_a_generated_pack_has_no_table() {
        // 1.8.9 and every release but the prototype one.
        assert!(table_for(47).is_none());
        assert!(table_for(767).is_none());
        assert!(table_for(999).is_none());
    }

    #[test]
    fn the_prototype_disguises_pale_oak_planks_on_1_20_1() {
        let table = table_for(763).expect("1.20.1 has a pack");
        assert_eq!(table.pack_format, 15, "1.20–1.20.1 is pack_format 15");
        let block = default_state("minecraft:pale_oak_planks").expect("engine has the block");
        let carrier = table.carrier(block).expect("pale oak planks is disguised");
        // The carrier is a real note_block state the engine knows: a carrier
        // the engine lacks would have no stand-in on any client.
        let name = aether_world::registry::blocks::block_of_state(carrier).map(|(_, n)| n);
        assert_eq!(name, Some("minecraft:note_block"), "expected a note block");
        assert_ne!(carrier, block);
        // The codec's own 1.20.1 block table lands that carrier on the matching
        // note state (harp, note 1, unpowered), which is the state the pack
        // retextures. The two halves of the layer must agree on the number.
        assert_eq!(super::super::V1_20_1.state(carrier), 541);
    }

    #[test]
    fn an_undisguised_block_is_unchanged() {
        let table = table_for(763).unwrap();
        let stone = aether_api::block_ids::STONE;
        assert_eq!(table.carrier(stone), None);
    }

    #[test]
    fn the_carrier_view_remaps_one_section_and_keeps_the_rest() {
        use crate::protocol::BlockSource;
        struct One(BlockStateId);
        impl BlockSource for One {
            fn block_at(&self, _: i32, _: i32, _: i32) -> BlockStateId {
                self.0
            }
        }
        let table = table_for(763).unwrap();
        let block = default_state("minecraft:pale_oak_planks").unwrap();
        let carrier = table.carrier(block).unwrap();
        let world = One(block);
        let view = CarrierView::new(&world, table);
        assert_eq!(view.block_at(0, 0, 0), carrier);
        // A section copy remaps identically.
        let mut out = [aether_api::block_ids::AIR; 4096];
        view.copy_section(0, 0, 0, &mut out);
        assert!(out.iter().all(|s| *s == carrier));
        // An unrelated state passes through.
        let stone = One(aether_api::block_ids::STONE);
        let plain = CarrierView::new(&stone, table);
        assert_eq!(plain.block_at(0, 0, 0), aether_api::block_ids::STONE);
    }
}
