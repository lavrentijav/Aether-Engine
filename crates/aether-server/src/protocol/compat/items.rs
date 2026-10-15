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
//! by [`ItemTable::resolve_change`], which follows the single shadowed cursor
//! item so the real item goes with the move, a report with no source is a
//! *creation* (the genuine item the wire names), and a stand-in cannot be
//! duplicated or conjured from the wire.
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

    /// Whether two engine items go out on the wire under the same id.
    ///
    /// The wire cannot tell a genuine item from a stand-in that shares its id
    /// (`oak_planks` and `pale_oak_planks`), so "is this the same thing on the
    /// client's screen?" is a wire-id question, not an engine-id one.
    #[inline]
    pub fn same_wire(&self, a: ItemTypeId, b: ItemTypeId) -> bool {
        match (self.wire(a), self.wire(b)) {
            (Some(x), Some(y)) => x.id == y.id,
            _ => a == b,
        }
    }
}

/// The outcome of resolving one creative slot report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    /// Write this item (or an empty slot) into the reported slot.
    Apply(Option<ItemTypeId>),
}

impl ItemTable {
    /// Resolve one creative slot report against the window's state.
    ///
    /// `report` is the item the client says the slot now holds (`None` for an
    /// emptied slot), `previous` is what the server had in that slot, and
    /// `carried` is the item shadowed from an earlier report — the server's
    /// model of the single item on the client's cursor. Returns the resolution
    /// and the carried item after the report.
    ///
    /// The wire id alone cannot say whether a report of a stand-in *is* the
    /// stand-in or the genuine item it hides. The server therefore:
    ///
    /// * follows the **carried** item first — the one source taken off a slot
    ///   by an earlier report. When the report names that stand-in the client
    ///   *placed* it: the slot takes the real carried item and the cursor takes
    ///   whatever the slot held (a swap), so the real item follows the move and
    ///   the shadow is neither duplicated nor stranded;
    /// * otherwise keeps the slot's **own** real item when the report names the
    ///   same wire id (a re-send, or an echo), so a spear shown as a trident
    ///   stays a spear even though the client can only say "trident";
    /// * otherwise treats the report as **creation**: a creative client can
    ///   conjure any item its version has, so the genuine item is written.
    ///
    /// The carried item is checked **before** the slot's own value on purpose
    /// (N1): a stand-in placed onto a slot that already shows the same wire id
    /// — a normal swap of a spear shown as a trident onto a genuine trident —
    /// is a *move*, not an echo. Reading it as an echo left the slot holding
    /// the genuine item and the spear only in the ephemeral cursor shadow, so
    /// it became a trident on relog. A redundant re-send of an unchanged slot
    /// while carrying is not something a client does; every `Set Creative Mode
    /// Slot` follows a click that changed the slot.
    ///
    /// An earlier design instead scanned every occupied slot for a source and
    /// refused a report with none. That both made ordinary creative picks of a
    /// stand-in-target item vanish *and* let a single source feed many
    /// placements (the source was never consumed), minting duplicates. The
    /// source is now only ever the shadow, and only ever once.
    pub fn resolve_change(
        &self,
        report: Option<ItemTypeId>,
        previous: Option<ItemTypeId>,
        carried: Option<ItemTypeId>,
    ) -> (Resolution, Option<ItemTypeId>) {
        match report {
            None => {
                // The slot emptied: the client picked up whatever was in it, so
                // the cursor now holds that item — shadow it whether it is a
                // stand-in *or* genuine. (A genuine item's wire id would usually
                // be enough on its own, but once it displaces a stand-in the
                // cursor is the only thing that can still tell the two apart.)
                // An already-empty slot reports nothing about the cursor, so a
                // duplicate empty report must **not** clear the shadow — that
                // would destroy the item (F4).
                let carried = if previous.is_some() {
                    previous
                } else {
                    carried
                };
                (Resolution::Apply(None), carried)
            }
            Some(r) => {
                // The client placed the item it was carrying. The slot takes
                // the real carried item; the cursor takes whatever the slot
                // held (a swap), or empties when the slot was empty. Checked
                // before `previous` so a swap onto a same-wire slot is a move,
                // not an echo (N1).
                if let Some(c) = carried.filter(|c| self.same_wire(*c, r)) {
                    return (Resolution::Apply(Some(c)), previous);
                }
                if let Some(p) = previous.filter(|p| self.same_wire(*p, r)) {
                    // The client re-sent the slot's own contents: nothing moved.
                    return (Resolution::Apply(Some(p)), carried);
                }
                // Creation: the client conjured (or replaced with) the genuine
                // item the wire id names. A shadow for a *different* wire id is
                // left alone — it is still on the cursor — and only a matching
                // report could have spent it.
                (Resolution::Apply(Some(r)), carried)
            }
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
        764 => &V1_20_2, // 1.20.2
        765 => &V1_20_4, // 1.20.3 / 1.20.4
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

    /// A tiny deterministic xorshift, so the property test is reproducible
    /// without a dependency.
    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    /// The multiset of real engine items across slots plus the cursor, so two
    /// windows can be compared exactly (a loss shows up as a missing entry).
    fn multiset(
        slots: &[Option<ItemTypeId>],
        cursor: Option<ItemTypeId>,
    ) -> std::collections::BTreeMap<ItemTypeId, usize> {
        let mut m = std::collections::BTreeMap::new();
        for it in slots.iter().flatten().chain(cursor.iter()) {
            *m.entry(*it).or_insert(0) += 1;
        }
        m
    }

    /// Drive a random sequence of creative window reports against a faithful
    /// client model and check the server's view never diverges from it.
    ///
    /// This is the property J10 must hold: the server's slots and its model of
    /// the cursor stay equal to the client's, so the multiset of real items is
    /// never changed except by an explicit creation of a genuine menu item. The
    /// engine-only spear can only move, so its count is invariant. With
    /// `allow_creation` false the whole multiset is conserved exactly.
    ///
    /// The model makes every report a *real* client action: a pick-up empties
    /// an occupied slot, a placement fills an empty one, a swap exchanges the
    /// cursor and an occupied slot, and a conjure fills an empty slot from the
    /// menu. There is no "echo" op — a client only sends a slot report after a
    /// click that changed that slot — which is what N1 turned on.
    fn run_reports(seed: u64, steps: usize, allow_creation: bool) {
        let t = &V1_21_1;
        let spear = id("minecraft:iron_spear");
        if shown_as(t, spear).is_none() {
            return;
        }
        // What the client actually receives for a real item (`engine()` of its
        // wire id): a spear arrives as a trident.
        let shown = |it: ItemTypeId| {
            t.engine(t.wire(it).expect("a version item").id)
                .expect("a real item")
        };
        let menu = [
            id("minecraft:trident"),
            id("minecraft:diamond"),
            id("minecraft:oak_planks"),
        ];

        let n = 9;
        let mut client: Vec<Option<ItemTypeId>> = vec![None; n];
        client[0] = Some(spear);
        client[4] = Some(spear);
        let mut cursor: Option<ItemTypeId> = None;
        let initial = multiset(&client, cursor);

        // The server's model: its slots and its shadow of the client's cursor.
        let mut server: Vec<Option<ItemTypeId>> = client.clone();
        let mut shadow: Option<ItemTypeId> = None;
        let mut rng = seed | 1;

        for step in 0..steps {
            let s = (xorshift(&mut rng) as usize) % n;
            let op = xorshift(&mut rng) % 8;
            match op {
                // Pick up an occupied slot (cursor empty).
                0..=2 if cursor.is_none() && client[s].is_some() => {
                    cursor = client[s].take();
                }
                // Place the carried item into an empty slot.
                3..=5 if cursor.is_some() && client[s].is_none() => {
                    client[s] = cursor.take();
                }
                // Swap the cursor with an occupied slot.
                6 if cursor.is_some() && client[s].is_some() => {
                    let held = cursor.take();
                    cursor = std::mem::replace(&mut client[s], held);
                }
                // Conjure a genuine menu item into an empty slot.
                7 if allow_creation && cursor.is_none() && client[s].is_none() => {
                    client[s] = Some(menu[(xorshift(&mut rng) as usize) % menu.len()]);
                }
                _ => continue,
            }
            // The client reports the slot's new value; fold the server's answer.
            let (res, carry) = t.resolve_change(client[s].map(shown), server[s], shadow);
            server[s] = match res {
                Resolution::Apply(i) => i,
            };
            shadow = carry;

            // The server's whole view must match the client's: no slot, and no
            // cursor item, may differ. This is what a loss or a wrong move
            // shows up as (N3) — the old test only counted spears, so any bug
            // that dropped an item passed.
            assert_eq!(server, client, "step {step}: server slots diverged");
            assert_eq!(shadow, cursor, "step {step}: server cursor diverged");

            // And the multiset of real items, count included, must never lose
            // anything; a spear (engine-only) may never be created.
            let now = multiset(&client, cursor);
            for (item, count) in &initial {
                assert!(
                    now.get(item).copied().unwrap_or(0) >= *count,
                    "step {step}: lost {item:?}"
                );
            }
            assert_eq!(
                now.get(&spear).copied().unwrap_or(0),
                initial[&spear],
                "step {step}: the spear count changed"
            );
            if !allow_creation {
                assert_eq!(
                    now, initial,
                    "step {step}: a pure move changed the multiset"
                );
            }
        }
    }

    #[test]
    fn random_window_reports_never_mint_a_real_item() {
        // The property J10 must hold: random creative window actions never
        // change the multiset of *real* engine items except by an explicit
        // creative action (a menu conjure of a genuine item), and the server's
        // view never diverges from the client's. The comparison is the full
        // multiset, count included, so a loss fails too (N3).
        for seed in 0..64u64 {
            run_reports(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), 400, true);
        }
        for seed in 0..64u64 {
            run_reports(seed.wrapping_mul(0xD1B5_4A32_D192_ED03), 400, false);
        }
    }

    #[test]
    fn a_legacy_block_item_follows_its_blocks_item_name() {
        // F7: on 1.8 a few blocks have no item of the same name. The sign and
        // banner blocks are placed by `sign`/`banner`, so a held sign must be
        // that item — stone was a generic fallback the `ITEM_FALLBACKS` test
        // could not see (stone is not a fallback, and the item *does* have a
        // stand-in).
        // The engine is 1.21.11, which has no generic `sign`/`banner` item, so
        // the assertion is on the *version* name the wire id carries.
        let t = &V1_8;
        for name in [
            "minecraft:oak_sign",
            "minecraft:spruce_sign",
            "minecraft:dark_oak_hanging_sign",
        ] {
            let w = t
                .wire(id(name))
                .unwrap_or_else(|| panic!("{name} stand-in"));
            assert!(w.disguised, "{name}");
            assert_eq!(t.name(w.id), Some("minecraft:sign"), "{name}");
        }
        for name in ["minecraft:white_banner", "minecraft:red_banner"] {
            let w = t.wire(id(name)).unwrap();
            assert_eq!(t.name(w.id), Some("minecraft:banner"), "{name}");
        }
        // 1.8 has no `fire` item; a campfire's block stand-in is `fire`, and
        // the item that makes fire there is flint and steel, so the campfire
        // item maps to that rather than stone (F7).
        for name in ["minecraft:campfire", "minecraft:soul_campfire"] {
            let w = t.wire(id(name)).unwrap();
            assert!(w.disguised, "{name}");
            assert_eq!(t.name(w.id), Some("minecraft:flint_and_steel"), "{name}");
        }
        // No block item whose block has a *rule* is left as stone: the only
        // block items that still show stone are those whose block also has no
        // rule (granite, deepslate, ...), where the block table is stone too.
        let stone = t.wire(id("minecraft:stone")).unwrap().id;
        for name in [
            "minecraft:oak_sign",
            "minecraft:white_banner",
            "minecraft:campfire",
            "minecraft:cherry_planks",
        ] {
            let w = t.wire(id(name)).unwrap();
            assert_ne!(w.id, stone, "{name} must not fall to stone");
        }
    }

    #[test]
    fn a_block_item_follows_its_blocks_substitution_not_the_item_rules() {
        // F5 (HIGH): a name that is both a block item and had an item rule was
        // resolved by the item rule first, so the held icon and the placed
        // block differed in shape — `resin_clump` showed a Honeycomb but placed
        // a Honeycomb Block. The item must take the block's stand-in item.
        let cases = [
            (
                &V1_21_1,
                "minecraft:resin_clump",
                "minecraft:honeycomb_block",
            ),
            (&V1_21_1, "minecraft:dried_ghast", "minecraft:white_wool"),
            (&V1_17, "minecraft:frogspawn", "minecraft:lily_pad"),
            (&V1_18, "minecraft:frogspawn", "minecraft:lily_pad"),
            (
                &V1_20_1,
                "minecraft:resin_clump",
                "minecraft:honeycomb_block",
            ),
        ];
        for (t, item, block) in cases {
            let w = t
                .wire(id(item))
                .unwrap_or_else(|| panic!("{item} has a stand-in"));
            assert!(w.disguised, "{item} is a disguise");
            assert_eq!(
                w.id,
                t.wire(id(block)).expect("the block's item").id,
                "{item} must show the block {block}, not the old item rule"
            );
        }
        // The old item rules are gone: neither name resolves to its former
        // generic target.
        assert_ne!(
            V1_21_1.wire(id("minecraft:resin_clump")).unwrap().id,
            V1_21_1.wire(id("minecraft:honeycomb")).unwrap().id
        );
        assert_ne!(
            V1_21_1.wire(id("minecraft:dried_ghast")).unwrap().id,
            V1_21_1.wire(id("minecraft:rotten_flesh")).unwrap().id
        );
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
            ("1.20.2", item_gen::v1_20_2::ITEM_FALLBACKS),
            ("1.20.4", item_gen::v1_20_4::ITEM_FALLBACKS),
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

    /// The stand-in the client sees for `real` on table `t`, if any.
    fn shown_as(t: &ItemTable, real: ItemTypeId) -> Option<ItemTypeId> {
        t.wire(real).and_then(|w| t.engine(w.id))
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
        let Some(shown) = shown_as(t, spear) else {
            return; // no spear rule: nothing to move
        };
        // 1. Pick up: slot 36 empties, and the spear is now on the cursor.
        let (res, carried) = t.resolve_change(None, Some(spear), None);
        assert_eq!(res, Resolution::Apply(None));
        assert_eq!(carried, Some(spear));

        // 2. Place into empty slot 10: the client reports the trident. The
        //    source slot is gone, so only the carried shadow identifies it.
        let (res, carried) = t.resolve_change(Some(shown), None, carried);
        assert_eq!(res, Resolution::Apply(Some(spear)));
        assert_eq!(carried, None, "the one source is spent");

        // 3. Pick up and place again, into slot 11.
        let (_, carried) = t.resolve_change(None, Some(spear), carried);
        let (res, _) = t.resolve_change(Some(shown), None, carried);
        assert_eq!(res, Resolution::Apply(Some(spear)));

        // 4. A trident with no source is a *creation*: a creative client can
        //    conjure one, so it is accepted as the genuine trident (F1).
        let (res, _) = t.resolve_change(Some(shown), None, None);
        assert_eq!(res, Resolution::Apply(Some(id("minecraft:trident"))));
        // ...and a genuine item is always believed.
        let (res, _) = t.resolve_change(Some(id("minecraft:diamond")), None, None);
        assert_eq!(res, Resolution::Apply(Some(id("minecraft:diamond"))));
    }

    #[test]
    fn a_single_source_cannot_feed_two_placements() {
        // F2 (HIGH): the old code scanned every occupied slot and never
        // consumed the source, so a client that simply never sent the
        // source-empty packet minted copies. Here slot 36 holds one spear; the
        // client claims a trident in slot 10 twice. On the second report the
        // shadow is spent, so it is a *creation* of a genuine trident — never
        // a second spear.
        let spear = id("minecraft:iron_spear");
        let t = &V1_21_1;
        let Some(shown) = shown_as(t, spear) else {
            return;
        };
        let (res, carried) = t.resolve_change(Some(shown), None, None);
        assert_eq!(
            res,
            Resolution::Apply(Some(id("minecraft:trident"))),
            "no source at all: a creation"
        );
        assert_eq!(carried, None);

        // A real move: pick the spear up, place it, then try to place again.
        let (_, carried) = t.resolve_change(None, Some(spear), None);
        let (first, carried) = t.resolve_change(Some(shown), None, carried);
        assert_eq!(first, Resolution::Apply(Some(spear)));
        assert_eq!(carried, None);
        let (second, _) = t.resolve_change(Some(shown), None, carried);
        assert_eq!(
            second,
            Resolution::Apply(Some(id("minecraft:trident"))),
            "the source was spent, so the second report is a genuine creation"
        );
    }

    #[test]
    fn an_empty_report_for_an_empty_slot_does_not_destroy_the_shadow() {
        // F4 (MEDIUM): the old `None` arm set `carried = previous`
        // unconditionally, so a client re-sending an empty stack for an
        // already-empty slot cleared the shadow and the real item was lost.
        let spear = id("minecraft:iron_spear");
        let t = &V1_21_1;
        let Some(shown) = shown_as(t, spear) else {
            return;
        };
        // Pick the spear up: the shadow is now the spear.
        let (_, carried) = t.resolve_change(None, Some(spear), None);
        assert_eq!(carried, Some(spear));
        // An empty report for a slot that was *already* empty says nothing
        // about the cursor, so the shadow must survive.
        let (res, carried) = t.resolve_change(None, None, carried);
        assert_eq!(res, Resolution::Apply(None));
        assert_eq!(carried, Some(spear), "the spear must not be lost");
        // And it can still be placed as the real item.
        let (res, _) = t.resolve_change(Some(shown), None, carried);
        assert_eq!(res, Resolution::Apply(Some(spear)));
    }

    #[test]
    fn the_source_is_the_carried_item_not_the_lowest_slot() {
        // F6 (MEDIUM): picking the first slot whose wire id matched made the
        // result depend on slot order. With a genuine trident in slot 9 and a
        // spear in slot 36, a placement that moved the spear must yield the
        // spear, not the trident (and vice versa), regardless of order.
        let spear = id("minecraft:iron_spear");
        let trident = id("minecraft:trident");
        let t = &V1_21_1;
        let Some(shown) = shown_as(t, spear) else {
            return;
        };
        // The shadow is what the client carried, not any other slot.
        let (res, _) = t.resolve_change(Some(shown), None, Some(spear));
        assert_eq!(res, Resolution::Apply(Some(spear)));
        // A genuine item carried is preserved as itself, even with a spear
        // elsewhere in the window.
        let (res, _) = t.resolve_change(Some(shown), None, Some(trident));
        assert_eq!(res, Resolution::Apply(Some(trident)));
        // No shadow + a genuine trident already in the destination slot is an
        // echo of that slot, not the spear.
        let (res, _) = t.resolve_change(Some(trident), Some(trident), None);
        assert_eq!(res, Resolution::Apply(Some(trident)));
    }

    #[test]
    fn a_swap_of_a_stand_in_onto_a_same_wire_slot_keeps_the_real_item() {
        // N1: pick up a spear S (shown as a trident) and left-click-swap it
        // onto a slot that already holds a genuine trident T. Both show the
        // same wire id, so the report looks like an echo. Reading it as one
        // stranded S in the ephemeral cursor shadow and left T in the slot —
        // S became a trident on relog, the exact outcome the layer exists to
        // prevent. The placement must win: slot 10 takes S, the cursor takes T.
        let spear = id("minecraft:iron_spear");
        let trident = id("minecraft:trident");
        let t = &V1_21_1;
        let Some(shown) = shown_as(t, spear) else {
            return;
        };
        assert_eq!(shown, trident, "the spear is shown as a trident");

        // 1. Slot 36 holds the spear, slot 10 a genuine trident.
        let mut slot36 = Some(spear);
        let mut slot10 = Some(trident);
        let mut carried = None;

        // 2. Left-click slot 36: pick the spear up.
        let (res, carry) = t.resolve_change(None, slot36, carried);
        assert_eq!(res, Resolution::Apply(None));
        slot36 = None;
        carried = carry;
        assert_eq!(carried, Some(spear));

        // 3. Left-click slot 10: swap the spear onto the trident. The packet
        //    says "trident", which matches *both* slots.
        let (res, carry) = t.resolve_change(Some(shown), slot10, carried);
        match res {
            Resolution::Apply(i) => slot10 = i,
        }
        carried = carry;
        assert_eq!(slot10, Some(spear), "the spear must land in slot 10");
        assert_eq!(carried, Some(trident), "the trident is now on the cursor");

        // No item is created or lost: exactly one spear and one trident.
        let all = [slot36, slot10, carried];
        assert_eq!(all.iter().filter(|i| **i == Some(spear)).count(), 1);
        assert_eq!(all.iter().filter(|i| **i == Some(trident)).count(), 1);

        // A re-send of an *unrelated* slot while carrying is still an echo.
        let (res, carry) = t.resolve_change(Some(trident), Some(trident), carried);
        assert_eq!(res, Resolution::Apply(Some(trident)));
        assert_eq!(carry, Some(trident), "an echo does not spend the shadow");
    }

    #[test]
    fn a_creative_pick_of_a_stand_in_target_item_is_not_refused() {
        // F1: many ordinary items share their wire id with some engine item's
        // stand-in — `oak_planks` is `pale_oak_planks`'s, `torch` is
        // `copper_torch`'s — so a plain creative pick from the menu is a
        // stand-in with no source. It must be written as the genuine item, not
        // refused (which made Ordinary Oak Planks unobtainable on 1.20.5–1.21.8).
        let t = &V1_21_1;
        for name in [
            "minecraft:oak_planks",
            "minecraft:torch",
            "minecraft:bookshelf",
            "minecraft:iron_bars",
            "minecraft:trident",
        ] {
            let picked = id(name);
            let w = t.wire(picked).expect("a version item");
            assert!(
                t.stand_in_wire(w.id),
                "{name} must be a stand-in wire id for this test to bite"
            );
            let (res, carry) = t.resolve_change(Some(picked), None, None);
            assert_eq!(res, Resolution::Apply(Some(picked)), "{name} vanished");
            assert_eq!(carry, None);
        }
    }

    #[test]
    fn the_item_table_is_found_by_protocol_and_absent_for_the_engine_version() {
        assert!(table_for(767).is_some());
        assert!(table_for(47).is_some());
        // 1.20.2/1.20.4 ship their own codecs and item tables (N2).
        assert!(table_for(764).is_some());
        assert!(table_for(765).is_some());
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
