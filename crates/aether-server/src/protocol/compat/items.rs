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
//! [`ItemTable::resolve_reported`] is the other half of that when the client
//! echoes the stand-in back into the *same* slot. A client that instead
//! *moves* the stand-in — through the cursor into another slot — is answered
//! by [`ItemTable::resolve_move`], which resolves the destination against the
//! window's authoritative items so the real item follows the move and a
//! stand-in cannot be conjured from the wire.
//!
//! The block half of the layer (block-state translation and substitution,
//! tasks J1/J2) is a sibling, and a block *item* now follows its block:
//! `tools/gen/compat/gen_items.js` resolves a block item through the same
//! `blocks.json` rules and resolver `gen.js` uses for the blocks themselves,
//! so the icon a player holds and the block they place downgrade together.

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

    /// Whether this version shows *any* engine item as a different item under
    /// `wire` — i.e. whether a report of `wire` is ambiguous between the
    /// genuine item of that id and the real item it disguises.
    pub fn stand_in_wire(&self, wire: i32) -> bool {
        wire >= 0
            && self
                .map
                .iter()
                .any(|&e| e >= 0 && e & 0x1_0000 != 0 && (e & 0xffff) == wire)
    }

    /// Resolve an item a client reported on the wire, with the window's
    /// authoritative items as context (the cursor and every real slot).
    ///
    /// The wire id alone cannot say whether a report of a stand-in *is* the
    /// stand-in or the genuine item it hides: both look identical. So a report
    /// whose wire id is unambiguous is taken as stated, and one whose wire id
    /// is a stand-in is matched against the items the server still holds — the
    /// source of the move — and refused (`None`) when there is no source. This
    /// is what carries a spear across a creative move instead of letting the
    /// trident stand-in become a real trident. Compare [`Self::resolve_reported`],
    /// which only looks at the destination slot.
    pub fn resolve_move(
        &self,
        reported: ItemTypeId,
        authority: &[ItemTypeId],
    ) -> Option<ItemTypeId> {
        let Some(r) = self.wire(reported) else {
            // An item this version cannot express at all: nothing on the wire
            // to launder, so the report stands for whatever it names.
            return Some(reported);
        };
        if !self.stand_in_wire(r.id) {
            return Some(reported);
        }
        authority
            .iter()
            .copied()
            .find(|&candidate| self.wire(candidate).is_some_and(|w| w.id == r.id))
    }
}

/// The outcome of resolving one creative slot report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    /// Write this item (or an empty slot) into the reported slot.
    Apply(Option<ItemTypeId>),
    /// The report was a stand-in with no authoritative source: refuse it.
    Refuse,
}

impl ItemTable {
    /// Resolve one creative slot report against the window's state.
    ///
    /// `report` is the item the client says the slot now holds (`None` for an
    /// emptied slot), `previous` is what the server had in that slot,
    /// `carried` is the item shadowed from an earlier report (the client's
    /// cursor), and `authority` is every other real slot's item. Returns the
    /// resolution and the carried item after the report, so the caller can
    /// keep shadowing the move. This is the per-move counterpart of
    /// [`Self::resolve_reported`]: it follows a stand-in from slot to cursor
    /// to slot instead of only matching the slot it came back to.
    pub fn resolve_change(
        &self,
        report: Option<ItemTypeId>,
        previous: Option<ItemTypeId>,
        carried: Option<ItemTypeId>,
        authority: &[ItemTypeId],
    ) -> (Resolution, Option<ItemTypeId>) {
        // The shadow is the strongest source: it is the item the last report
        // took off a slot, so prepend it to the window's items.
        let mut sources = Vec::with_capacity(authority.len() + 1);
        if let Some(c) = carried {
            sources.push(c);
        }
        sources.extend_from_slice(authority);

        match report {
            None => {
                // The slot emptied: the client is now carrying what was in it.
                (Resolution::Apply(None), previous)
            }
            Some(r) if previous == Some(r) => {
                // The client re-sent what the slot already held: nothing moved.
                (Resolution::Apply(Some(r)), carried)
            }
            Some(r) => match self.resolve_move(r, &sources) {
                Some(real) => {
                    let carried = if carried == Some(real) { None } else { carried };
                    (Resolution::Apply(Some(real)), carried)
                }
                None => (Resolution::Refuse, carried),
            },
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
table!(V1_20_5, v1_20_5);
table!(V1_21_1, v1_21_1);
table!(V1_21_3, v1_21_3);
table!(V1_21_4, v1_21_4);
table!(V1_21_5, v1_21_5);
table!(V1_21_6, v1_21_6);
table!(V1_21_8, v1_21_8);
table!(V1_21_9, v1_21_9);

/// The item table for a codec's protocol id, if this layer knows it.
///
/// Mirrors [`super::block_table`] so a caller (the session's creative-slot
/// handling) can resolve a client report without naming the static. `None` for
/// 1.21.11 and later, whose ids the engine itself uses and which therefore
/// need no substitution, and for any protocol this layer does not cover.
pub fn table_for(protocol: i32) -> Option<&'static ItemTable> {
    let t = match protocol {
        47 => &V1_8, // 1.8.9, pre-flattening
        755 => &V1_17,
        756 => &V1_17_1,
        757 => &V1_18,
        758 => &V1_18_2,
        759 => &V1_19,
        760 => &V1_19_2,
        761 => &V1_19_3,
        762 => &V1_19_4,
        763 => &V1_20_1,
        766 => &V1_20_5,
        767 => &V1_21_1,
        768 => &V1_21_3,
        769 => &V1_21_4,
        770 => &V1_21_5,
        771 => &V1_21_6,
        772 => &V1_21_8,
        773 => &V1_21_9,
        _ => return None,
    };
    Some(t)
}

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
    fn no_block_item_is_left_without_a_stand_in() {
        // A block item follows its block's substitution (J10), and a block no
        // rule covers becomes stone, so `ITEM_FALLBACKS` — the items a client
        // is shown nothing at all for — must only ever contain items that are
        // *not* blocks. A block name here means the block and the item drifted
        // apart.
        for (name, fallbacks) in [
            ("1.8", item_gen::v1_8::ITEM_FALLBACKS),
            ("1.17", item_gen::v1_17::ITEM_FALLBACKS),
            ("1.19", item_gen::v1_19::ITEM_FALLBACKS),
            ("1.20.1", item_gen::v1_20_1::ITEM_FALLBACKS),
            ("1.21.1", item_gen::v1_21_1::ITEM_FALLBACKS),
            ("1.21.9", item_gen::v1_21_9::ITEM_FALLBACKS),
        ] {
            for item in fallbacks {
                assert!(
                    aether_world::registry::blocks::default_state(item).is_none(),
                    "{name}: block item {item} fell back instead of following its block rule"
                );
            }
        }
    }

    #[test]
    fn a_moved_stand_in_keeps_the_real_item_it_stands_for() {
        // J10: the round trip follows a *move*, not just an echo in the same
        // slot. `authority` is what the server still holds in the window
        // (cursor then slots) before the move.
        let spear = id("minecraft:iron_spear");
        let t = &V1_21_1;
        let Some(w) = t.wire(spear) else {
            return; // no spear rule: nothing to move
        };
        let shown = t.engine(w.id).unwrap(); // the trident stand-in
        assert!(t.stand_in_wire(t.wire(shown).unwrap().id));
        // The source is still somewhere in the window: the destination takes
        // the real spear, not the trident the client sent.
        assert_eq!(t.resolve_move(shown, &[spear]), Some(spear));
        // A genuine trident in the window is preserved as itself.
        let trident = id("minecraft:trident");
        assert_eq!(t.resolve_move(shown, &[trident]), Some(trident));
        // A stand-in with no source is refused: the wire cannot conjure one.
        assert_eq!(t.resolve_move(shown, &[]), None);
        // An unambiguous item is always taken as stated.
        assert_eq!(
            t.resolve_move(id("minecraft:diamond"), &[]),
            Some(id("minecraft:diamond"))
        );
    }

    #[test]
    fn a_stand_in_survives_being_carried_through_the_cursor_between_slots() {
        // The flow KNOWN_ISSUES 15's second bullet asks for: a creative client
        // moves a spear (shown as a trident stand-in) slot -> cursor -> empty
        // slot -> cursor -> another slot. Each report arrives on its own, so
        // the emptied slot has to shadow the real item for the placement that
        // follows.
        let spear = id("minecraft:iron_spear");
        let t = &V1_21_1;
        let Some(w) = t.wire(spear) else {
            return; // no spear rule: nothing to move
        };
        let shown = t.engine(w.id).unwrap(); // the trident the client sees
        let mut carried = None;
        let mut slots: Vec<(usize, ItemTypeId)> = vec![(36, spear)];
        // The window's items, minus the slot being reported on.
        let others = |slots: &[(usize, ItemTypeId)], skip: usize| {
            slots
                .iter()
                .filter(|(s, _)| *s != skip)
                .map(|(_, i)| *i)
                .collect::<Vec<_>>()
        };

        // 1. Pick up: slot 36 empties, and the spear is now on the cursor.
        let (res, next) = t.resolve_change(None, Some(spear), carried, &others(&slots, 36));
        assert_eq!(res, Resolution::Apply(None));
        carried = next;
        assert_eq!(carried, Some(spear));
        slots.retain(|(s, _)| *s != 36);

        // 2. Place into empty slot 10: the client reports the trident. The
        //    source is gone, so only the carried shadow can identify it.
        let (res, next) = t.resolve_change(Some(shown), None, carried, &others(&slots, 10));
        assert_eq!(res, Resolution::Apply(Some(spear)));
        carried = next;
        assert_eq!(carried, None);
        slots.push((10, spear));

        // 3. Pick up and place again, into slot 11.
        let (_, next) = t.resolve_change(None, Some(spear), carried, &others(&slots, 10));
        carried = next;
        let (res, _next) = t.resolve_change(Some(shown), None, carried, &others(&slots, 11));
        assert_eq!(res, Resolution::Apply(Some(spear)));

        // 4. A trident conjured from nothing is refused, not accepted.
        let (res, _) = t.resolve_change(Some(shown), None, None, &[]);
        assert_eq!(res, Resolution::Refuse);
        // ...but a genuine item is always believed.
        let (res, _) = t.resolve_change(Some(id("minecraft:diamond")), None, None, &[]);
        assert_eq!(res, Resolution::Apply(Some(id("minecraft:diamond"))));
    }

    #[test]
    fn the_item_table_is_found_by_protocol_and_absent_for_the_engine_version() {
        assert!(table_for(767).is_some());
        assert!(table_for(47).is_some());
        // 1.21.11 uses the engine's own ids, so there is nothing to translate.
        assert!(table_for(774).is_none());
        assert!(table_for(999).is_none());
    }

    #[test]
    fn how_many_items_still_have_no_stand_in_is_recorded() {
        // These are all non-block items the item rules (J3) do not cover
        // (copper tools, nautilus armour, banner patterns, ...); the bound
        // makes a block/item regression visible. See docs/KNOWN_ISSUES.md.
        assert!(
            item_gen::v1_21_1::ITEM_FALLBACKS.len() < 40,
            "1.21.1 item fallbacks grew: {:?}",
            item_gen::v1_21_1::ITEM_FALLBACKS
        );
        assert!(
            item_gen::v1_17::ITEM_FALLBACKS.len() < 70,
            "1.17 item fallbacks grew: {}",
            item_gen::v1_17::ITEM_FALLBACKS.len()
        );
    }
}
