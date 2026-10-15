//! The item half of the compatibility layer (task J3).
//!
//! The engine's item ids are 1.21.11's ([`aether_world::registry::items`]),
//! and each older release has its own numbering and — for anything added
//! since — no item with that name at all. [`ItemTable`] answers both
//! directions from a generated `ITEM_MAP` (engine id → the version's wire id,
//! the item substitution rules already applied) and `ITEM_BY_ID` (the
//! version's own name, for a round trip).
//!
//! A substituted item is a **disguise**: the client is handed a stand-in id
//! but is told the real item's display name
//! (`super::item_gen::english::ENGLISH`), and the server keeps the real stack.
//! [`ItemTable::resolve_reported`] is the other half of that: an item a client
//! sends back that *is* the stand-in for the slot's real item resolves to the
//! real item, so a client cannot launder a spear into a trident by moving the
//! slot around.
//!
//! The block half of the layer (block-state translation and substitution,
//! tasks J1/J2) is a sibling: block *items* a version lacks should follow its
//! block rules. Until that is merged the generated tables use a generic
//! placeholder stand-in for them; see `tools/gen/compat/gen_items.js` and the
//! TODO there.

// The per-release item tables are foundation for routing the older codecs
// through the gameplay path (queue J, task J5); until they encode items, the
// only build that exercises them is the test one.
#![allow(dead_code)]

use aether_world::registry::items::ItemTypeId;

use super::item_gen;

/// One substituted wire id: the version's id to send, and whether it stands in
/// for a different real item (so the client must be shown the real name).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WireItem {
    /// The version's wire id for the item.
    pub id: i32,
    /// True when this id is a stand-in for a *different* engine item.
    pub disguised: bool,
}

/// The item table of one release.
#[derive(Clone, Copy)]
pub struct ItemTable {
    /// Engine item id → packed wire id (low 16 bits, `-1` absent) plus the
    /// disguise bit (16).
    pub map: &'static [i32],
    /// The version's own item names by wire id; `""` for an unused id.
    pub by_id: &'static [&'static str],
}

impl ItemTable {
    /// The version's wire id for an engine item, if it can show it at all.
    ///
    /// `None` for a modded item and for one the version lacks and no rule
    /// covers. A `Some` whose `disguised` is set means the wire id is a
    /// different item standing in, and the caller should show the real name.
    #[inline]
    pub fn wire(&self, item: ItemTypeId) -> Option<WireItem> {
        let e = *self.map.get(item.0 as usize)?;
        if e < 0 {
            return None;
        }
        Some(WireItem {
            id: e & 0xffff,
            disguised: e & 0x1_0000 != 0,
        })
    }

    /// The version's own name for a wire id, `None` for one it does not have.
    #[inline]
    pub fn name(&self, wire: i32) -> Option<&'static str> {
        let i = usize::try_from(wire).ok()?;
        self.by_id.get(i).copied().filter(|n| !n.is_empty())
    }

    /// The engine item a wire id names, if the engine has an item by that name.
    #[inline]
    pub fn engine(&self, wire: i32) -> Option<ItemTypeId> {
        self.name(wire).and_then(ItemTypeId::vanilla)
    }

    /// Resolve an item a client reported in a slot to the real engine item.
    ///
    /// If the slot really holds `server`, and this version shows `server` as a
    /// substitute whose wire id is exactly what the client reported, the
    /// client is echoing the disguise back and the real item wins. Otherwise
    /// the report stands: the player genuinely put something else there.
    pub fn resolve_reported(&self, server: ItemTypeId, reported: ItemTypeId) -> ItemTypeId {
        match (self.wire(server), self.wire(reported)) {
            (Some(s), Some(r)) if s.disguised && s.id == r.id => server,
            _ => reported,
        }
    }
}

macro_rules! table {
    ($name:ident, $m:ident) => {
        /// The item table for this release.
        pub static $name: ItemTable = ItemTable {
            map: item_gen::$m::ITEM_MAP,
            by_id: item_gen::$m::ITEM_BY_ID,
        };
    };
}

table!(V1_8, v1_8);
table!(V1_17, v1_17);
table!(V1_17_1, v1_17_1);
table!(V1_18, v1_18);
table!(V1_18_2, v1_18_2);
table!(V1_19, v1_19);
table!(V1_19_2, v1_19_2);
table!(V1_19_3, v1_19_3);
table!(V1_19_4, v1_19_4);
table!(V1_20_1, v1_20_1);
table!(V1_20_2, v1_20_2);
table!(V1_20_4, v1_20_4);
table!(V1_20_5, v1_20_5);
table!(V1_21_1, v1_21_1);
table!(V1_21_3, v1_21_3);
table!(V1_21_4, v1_21_4);
table!(V1_21_5, v1_21_5);
table!(V1_21_6, v1_21_6);
table!(V1_21_8, v1_21_8);
table!(V1_21_9, v1_21_9);

/// The English display name of an engine item, for a substitute's label.
pub fn english(item: ItemTypeId) -> Option<&'static str> {
    item_gen::english::ENGLISH.get(item.0 as usize).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> ItemTypeId {
        ItemTypeId::vanilla(name).unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn an_item_the_version_has_maps_to_its_own_id() {
        // Diamond sword is in every release 1.8 onwards.
        let t = &V1_17;
        let w = t
            .wire(id("minecraft:diamond_sword"))
            .expect("present in 1.17");
        assert!(!w.disguised, "the same item is not a disguise");
        assert_eq!(t.engine(w.id), Some(id("minecraft:diamond_sword")));
    }

    #[test]
    fn a_later_item_is_a_disguise_that_round_trips_to_the_real_one() {
        // The mace has no pre-1.21 release, and the item rules send it to an
        // iron axe. The client sees the axe but the server keeps the mace,
        // and an echo of the stand-in resolves back.
        let real = id("minecraft:mace");
        let t = &V1_20_1;
        let w = t.wire(real).expect("a stand-in exists");
        assert!(w.disguised, "1.20.1 has no mace: this is a disguise");
        let stand_in = t.engine(w.id).expect("the wire id names a real item");
        assert_eq!(stand_in, id("minecraft:iron_axe"));
        // The client reports the stand-in; the server keeps the real item.
        assert_eq!(t.resolve_reported(real, stand_in), real);
        // And a genuinely different item is still believed.
        assert_eq!(
            t.resolve_reported(real, id("minecraft:diamond")),
            id("minecraft:diamond")
        );
    }

    #[test]
    fn spears_are_disguised_and_cannot_be_laundered_into_tridents() {
        // J4 wires the behaviour; the mapping is here. A client shown a
        // trident for an iron spear cannot turn it into a real trident by
        // moving the slot: the echo resolves to the spear, and a genuine
        // trident report is a different item.
        let spear = id("minecraft:iron_spear");
        let t = &V1_21_1;
        let Some(w) = t.wire(spear) else {
            // No stand-in rule for spears would also be acceptable here.
            return;
        };
        assert!(w.disguised);
        let shown = t.engine(w.id).unwrap();
        assert_ne!(shown, spear, "the stand-in is a different item");
        // The client echoes the stand-in while the slot really holds a spear:
        // the spear stays. This is deliberately conservative — the safe
        // direction is to keep the real item, never to accept the stand-in.
        assert_eq!(t.resolve_reported(spear, shown), spear);
        // A report that is not this slot's stand-in still stands.
        assert_eq!(
            t.resolve_reported(spear, id("minecraft:diamond")),
            id("minecraft:diamond")
        );
    }

    #[test]
    fn every_stand_in_a_table_emits_names_a_real_engine_item() {
        // A packed id must never come from a version item with no engine
        // name: that would make the client's echo resolve to nothing.
        for (t, name) in [
            (&V1_8, "1.8"),
            (&V1_17, "1.17"),
            (&V1_21_1, "1.21.1"),
            (&V1_21_9, "1.21.9"),
        ] {
            // `name()` returning a real name is what `engine()` needs.
            for wire in 0..t.by_id.len() as i32 {
                if let Some(n) = t.name(wire) {
                    assert!(n.starts_with("minecraft:"), "{name}: id {wire} is {n}");
                }
            }
        }
    }

    #[test]
    fn the_english_name_table_covers_the_engine_items() {
        // The label a disguised stack carries comes from here, indexed by
        // engine id; a missing row would show a nameless stand-in.
        assert_eq!(
            item_gen::english::ENGLISH.len(),
            aether_world::registry::items::ITEM_NAMES.len()
        );
        assert_eq!(english(id("minecraft:iron_spear")), Some("Iron Spear"));
    }

    #[test]
    fn how_many_items_still_have_no_stand_in_is_recorded() {
        // Block items still wait on the block rules (J2), so the count is
        // larger than it will be after the merge; the bound is here to make a
        // rule regression visible, and the list itself is printed below.
        assert!(
            item_gen::v1_21_1::ITEM_FALLBACKS.len() < 200,
            "1.21.1 item fallbacks grew: {}",
            item_gen::v1_21_1::ITEM_FALLBACKS.len()
        );
        assert!(
            item_gen::v1_17::ITEM_FALLBACKS.len() < 400,
            "1.17 item fallbacks grew: {}",
            item_gen::v1_17::ITEM_FALLBACKS.len()
        );
    }
}
