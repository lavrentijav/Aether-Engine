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
//! item *only when the report reconciles with it* (same wire id and a count it
//! could have carried); a report it cannot explain is a *creation* that
//! invalidates the shadow, so a stand-in cannot be duplicated, conjured from
//! the wire, or materialised after the client replaced its cursor unseen.
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

    /// Whether the client sees `item` as stackable.
    ///
    /// It decides whether two look-alike stacks merge or swap: the client piles
    /// two stackable items of the same wire id together, but swaps two
    /// unstackable ones (a spear and a trident it cannot tell apart). The client
    /// stacks by the item it is *shown*, so a stand-in that changes stackability
    /// is judged by the stand-in's size, not the real item's: a horse harness
    /// (stack 1) shown as leather (stack 64) is stacked by the client as leather,
    /// and must merge, not swap.
    #[inline]
    fn wire_stacks(&self, item: ItemTypeId) -> bool {
        let shown = self
            .wire(item)
            .and_then(|w| self.engine(w.id))
            .unwrap_or(item);
        crate::game::tables::max_stack_id(shown) > 1
    }
}

/// The item on the client's cursor, as the server models it: the real engine
/// item and how many of it remain.
///
/// The count matters because a placement can consume only part of a stack (a
/// right-click places one, and `Set Creative Mode Slot` cannot tell the server
/// how many — the packet has no button), so the shadow must be spent a stack
/// at a time, not cleared wholesale (N5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Carried {
    /// The real engine item on the cursor.
    pub item: ItemTypeId,
    /// How many of it are left on the cursor.
    pub count: u8,
}

/// The outcome of resolving one creative slot report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    /// Write this stack, `(item, count)`, or `None` for an empty slot, into the
    /// reported slot.
    Apply(Option<(ItemTypeId, u8)>),
}

impl ItemTable {
    /// Resolve one creative slot report against the window's state.
    ///
    /// `report` is the stack the client says the slot now holds (`None` for an
    /// emptied slot), `previous` is what the server had in that slot, and
    /// `carried` is the cursor shadow — the server's model of the single stack
    /// on the client's cursor. Returns the slot's new contents and the cursor
    /// shadow after the report.
    ///
    /// The wire id alone cannot say whether a report of a stand-in *is* the
    /// stand-in or the genuine item it hides, and the server never sees the
    /// client's cursor directly. It therefore:
    ///
    /// * if the report is **reconciled** with the carried shadow — same wire id
    ///   and a count the shadow could have carried (no more than it held, or the
    ///   cursor merged into a slot that shows the same item *on the wire*) —
    ///   the report is that stack being placed. Whether the slot's item is the
    ///   same real item or merely a different real item that shares its wire id
    ///   (a disguised stack onto a genuine one), the slot **keeps its own
    ///   authoritative item** and only what was *added* is consumed from the
    ///   cursor, so a genuine stack is never swapped away for a stand-in. A slot
    ///   showing a *different* wire id is a swap: the replaced stack goes to the
    ///   cursor;
    /// * if it is not reconciled but the slot already shows the same wire id and
    ///   the count did not grow, keeps the slot's own real item (a re-send/echo)
    ///   and **discards** the shadow (the report is not the shadow's item, so the
    ///   cursor is not what the server thought);
    /// * otherwise treats the report as a **swap or creation** with a cursor the
    ///   server does not know: the slot takes the genuine reported item, and
    ///   whatever the slot held moves to the cursor (N4).
    ///
    /// A client can put a creative-menu item on its cursor with no packet, so
    /// the shadow can be stale. A report the shadow cannot explain — a count
    /// above what it carried into an empty or foreign slot, i.e. the client
    /// conjured a stand-in-wire item the server never had — **invalidates** the
    /// shadow rather than materialising the engine-only item (round 4): the
    /// genuine reported item is written, never more of the stand-in than was
    /// carried. A same-wire report with **no** shadow keeps the slot's real item
    /// (the echo guard restored from before N1, scoped to the no-carry case so
    /// it cannot swallow a genuine swap).
    pub fn resolve_change(
        &self,
        report: Option<(ItemTypeId, u8)>,
        previous: Option<(ItemTypeId, u8)>,
        carried: Option<Carried>,
    ) -> (Resolution, Option<Carried>) {
        match report {
            None => {
                // The slot emptied: the cursor now holds the whole slot. If it
                // was already empty, a duplicate report says nothing about the
                // cursor, so the shadow survives (F4).
                let carry = previous
                    .map(|(item, count)| Carried { item, count })
                    .or(carried);
                (Resolution::Apply(None), carry)
            }
            Some((r_item, r_count)) => {
                if let Some(c) = carried.filter(|c| self.reconciles(*c, r_item, r_count, previous))
                {
                    // The client placed the carried stack into this slot.
                    let (slot_item, carry) = match previous {
                        // A merge. The client cannot tell a stand-in from the
                        // real item it shares a wire id with: when that item is
                        // *stackable* it piles the two look-alikes together, so
                        // the slot keeps its own authoritative item and only
                        // what was added is consumed from the cursor (a
                        // disguised stack onto a genuine one must not swap the
                        // genuine stack away — round 5). A same real item is
                        // always a merge. Two *unstackable* look-alikes (a spear
                        // shown as a trident onto a genuine trident) are a swap,
                        // handled below.
                        Some((p_item, p_count))
                            if self.same_wire(p_item, r_item)
                                && (p_item == c.item || self.wire_stacks(c.item)) =>
                        {
                            let consumed = r_count.saturating_sub(p_count);
                            let left = c.count.saturating_sub(consumed);
                            (
                                p_item,
                                (left > 0).then_some(Carried {
                                    item: c.item,
                                    count: left,
                                }),
                            )
                        }
                        // A different item on the wire: the client swapped the
                        // slot's stack with its cursor, so the replaced stack
                        // goes to the cursor.
                        Some((p_item, p_count)) => (
                            c.item,
                            Some(Carried {
                                item: p_item,
                                count: p_count,
                            }),
                        ),
                        // Place into an empty slot: consume the placed count.
                        None => {
                            let left = c.count.saturating_sub(r_count);
                            (
                                c.item,
                                (left > 0).then_some(Carried {
                                    item: c.item,
                                    count: left,
                                }),
                            )
                        }
                    };
                    return (Resolution::Apply(Some((slot_item, r_count))), carry);
                }
                // The report does not match the shadow. If it is the slot's own
                // wire id *and* the count did not grow, the client re-sent the
                // slot (or only shrank it): keep the real item already there. A
                // grown count is a merge from a cursor the server never
                // shadowed, not an echo, so it falls through to the swap below.
                //
                // The shadow is *discarded* either way: the report is not the
                // shadow's item, so the client's cursor is not what the server
                // thought — it replaced the cursor behind the server's back. A
                // stale shadow must not survive to re-materialise the engine-only
                // item later (round 4).
                if let Some(p) =
                    previous.filter(|(p, pc)| self.same_wire(*p, r_item) && r_count <= *pc)
                {
                    return (Resolution::Apply(Some(p)), None);
                }
                // The server does not know the client's cursor: `r` is a
                // conjured genuine item or a swap whose cursor item differs from
                // the shadow. Either way the slot takes the genuine reported
                // item and the slot's old item leaves to the cursor (N4).
                let carry = previous.map(|(item, count)| Carried { item, count });
                (Resolution::Apply(Some((r_item, r_count))), carry)
            }
        }
    }

    /// Whether the carried shadow can explain a slot report: same wire id, and
    /// a count it could have carried there.
    ///
    /// A count no greater than the shadow is a move or a partial placement. A
    /// larger count is only explicable as the cursor merged into a slot that
    /// shows the same item *on the wire* (the client cannot tell a stand-in from
    /// the real item it shares an id with, so it piles them together), bounded
    /// by what the shadow carried. Anything else is a cursor the client replaced
    /// behind the server's back, and the shadow is not trusted.
    fn reconciles(
        &self,
        carried: Carried,
        r_item: ItemTypeId,
        r_count: u8,
        previous: Option<(ItemTypeId, u8)>,
    ) -> bool {
        if !self.same_wire(carried.item, r_item) {
            return false;
        }
        if r_count <= carried.count {
            return true;
        }
        previous.is_some_and(|(p_item, p_count)| {
            self.same_wire(p_item, r_item) && r_count <= p_count.saturating_add(carried.count)
        })
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

    /// The multiset of real engine items across slots plus the cursor, counting
    /// each instance by its stack count, so two windows compare exactly and a
    /// loss shows up as a missing entry.
    fn multiset(
        slots: &[Option<(ItemTypeId, u8)>],
        cursor: Option<(ItemTypeId, u8)>,
    ) -> std::collections::BTreeMap<ItemTypeId, u64> {
        let mut m = std::collections::BTreeMap::new();
        for (item, count) in slots.iter().flatten().chain(cursor.iter()) {
            *m.entry(*item).or_insert(0) += *count as u64;
        }
        m
    }

    /// Drive a random sequence of creative window actions against a faithful
    /// client model and check the server never loses a real item or diverges on
    /// a slot.
    ///
    /// The model makes every action a *real* client gesture: a pick-up empties
    /// an occupied slot, a full placement fills an empty one, a single-item
    /// placement (a right-click) moves one, a swap exchanges the cursor and an
    /// occupied slot, and a **conjure onto the cursor** puts a genuine menu item
    /// on the cursor with no packet (N4) — the case the old model could not
    /// reach because its cursor was always shadowed. There is no "echo" op: a
    /// client only reports a slot after a click that changed it.
    ///
    /// Invariants after every server-visible action:
    ///
    /// * `server == client` — every slot holds the same real item and count;
    /// * the server's slots plus its cursor shadow hold every *disguised* item
    ///   the client does (a real item can never be lost to the disguise);
    /// * a disguised item on the client's cursor is mirrored in the shadow;
    /// * with no conjures, the whole multiset is conserved (N3, count included).
    fn run_reports(seed: u64, steps: usize, allow_creation: bool) {
        let t = &V1_21_1;
        let spear = id("minecraft:iron_spear");
        let pale = id("minecraft:pale_oak_planks");
        if shown_as(t, spear).is_none() || shown_as(t, pale).is_none() {
            return;
        }
        // What the client actually receives for a real item (`engine()` of its
        // wire id): a spear arrives as a trident.
        let shown = |it: ItemTypeId| {
            t.engine(t.wire(it).expect("a version item").id)
                .expect("a real item")
        };
        let max_of = crate::game::tables::max_stack_id;
        // The multiset of *wire* items across slots plus the cursor: what the
        // client can actually see. A same-wire merge converts a disguised
        // stack's real identity into the slot's item, so the *real* multiset is
        // not conserved, but the wire view a pure move preserves exactly is.
        let wire_multi = |slots: &[Option<(ItemTypeId, u8)>], cur: Option<(ItemTypeId, u8)>| {
            let mut m = std::collections::BTreeMap::new();
            for (it, c) in slots.iter().flatten().chain(cur.iter()) {
                *m.entry(shown(*it)).or_insert(0u64) += *c as u64;
            }
            m
        };

        // Menu items whose wire id does *not* collide with a stand-in. A
        // conjured item that shares a stand-in's wire id and a count the stale
        // shadow can cover is the documented *indistinguishable* residual (the
        // bytes equal a legitimate move), so the model does not conjure one;
        // the wire-colliding genuine items below are seeded instead, which is
        // where the merge bug lives.
        let menu = [
            id("minecraft:diamond"),
            id("minecraft:stick"),
            id("minecraft:cobblestone"),
            id("minecraft:apple"),
        ];
        for m in menu {
            assert!(!t.stand_in_wire(t.wire(m).unwrap().id), "menu item {m:?}");
        }

        // A stand-in that changes stackability: a horse harness (stack 1) is
        // shown to these clients as leather (stack 64), so the client stacks it
        // as leather and must merge, not swap.
        let harness = id("minecraft:white_harness");
        assert_eq!(shown_as(t, harness), Some(id("minecraft:leather")));

        let n = 9;
        let mut client: Vec<Option<(ItemTypeId, u8)>> = vec![None; n];
        client[0] = Some((spear, 1));
        client[2] = Some((id("minecraft:diamond"), 10));
        client[4] = Some((pale, 64));
        // Genuine items whose wire id a stand-in also uses (`oak_planks` with
        // pale_oak_planks, `trident` with the spear, `leather` with a harness):
        // the client sees the carried disguised stack and such a slot as one
        // kind and merges them.
        client[6] = Some((id("minecraft:oak_planks"), 11));
        client[8] = Some((id("minecraft:trident"), 1));
        client[1] = Some((harness, 1));
        client[7] = Some((id("minecraft:leather"), 16));
        let mut cursor: Option<(ItemTypeId, u8)> = None;
        let initial_wire = wire_multi(&client, cursor);

        let mut server = client.clone();
        let mut shadow: Option<Carried> = None;
        let mut rng = seed | 1;

        for step in 0..steps {
            let s = (xorshift(&mut rng) as usize) % n;
            let op = xorshift(&mut rng) % 9;
            let mut sent = false;
            match op {
                // Pick up an occupied slot (cursor empty).
                0..=2 if cursor.is_none() && client[s].is_some() => {
                    cursor = client[s].take();
                    sent = true;
                }
                // Place the whole carried stack into an empty slot.
                3..=4 if cursor.is_some() && client[s].is_none() => {
                    client[s] = cursor.take();
                    sent = true;
                }
                // Left-click an occupied slot with a non-empty cursor. The
                // client sees only the wire: two stacks of one stackable wire
                // item merge (the slot keeps its own real item, as the server
                // does); anything else — a different item, or two unstackable
                // look-alikes like a spear and a trident — swaps.
                5..=6 if cursor.is_some() && client[s].is_some() => {
                    let (held_item, held_count) = cursor.take().unwrap();
                    let (there_item, there_count) = client[s].unwrap();
                    let wire_max = max_of(shown(held_item));
                    if t.same_wire(held_item, there_item) && wire_max > 1 {
                        let sum = held_count as u16 + there_count as u16;
                        let on_slot = sum.min(wire_max as u16) as u8;
                        client[s] = Some((there_item, on_slot));
                        let left = sum - on_slot as u16;
                        cursor = if left > 0 {
                            Some((held_item, left as u8))
                        } else {
                            None
                        };
                    } else {
                        client[s] = Some((held_item, held_count));
                        cursor = Some((there_item, there_count));
                    }
                    sent = true;
                }
                // Place a single item from the cursor into an empty slot (a
                // right-click; the packet cannot say how many — N5).
                7 if cursor.is_some_and(|(_, c)| c > 1) && client[s].is_none() => {
                    let (item, count) = cursor.unwrap();
                    client[s] = Some((item, 1));
                    cursor = Some((item, count - 1));
                    sent = true;
                }
                // Conjure a genuine menu item onto the cursor (no packet). It
                // replaces whatever the client was carrying, so the server's
                // shadow — if any — is now stale (N4, round 4).
                8 if allow_creation => {
                    cursor = Some((menu[(xorshift(&mut rng) as usize) % menu.len()], 64));
                }
                _ => {}
            }
            if !sent {
                // A conjure (or a skipped op) sends no report: the slots must
                // still agree, but the cursor is now the server's blind spot.
                assert_eq!(server, client, "step {step}: server slots diverged");
                continue;
            }

            let report = client[s].map(|(item, count)| (shown(item), count));
            let (res, carry) = t.resolve_change(report, server[s], shadow);
            server[s] = match res {
                Resolution::Apply(stack) => stack,
            };
            shadow = carry;

            // Every slot holds the same real item and count on both sides.
            assert_eq!(server, client, "step {step}: server slots diverged");

            // Every *engine-only* item (a spear or pale_oak_planks, which the
            // client's creative menu cannot make) the client holds must be on
            // the server too — in a slot or in the cursor shadow. The shadow is
            // the only place the server tracks the cursor, so this is where a
            // loss or a fabrication of such an item shows up (N3, count
            // included). A genuinely conjured menu item's remainder on the
            // cursor is the client's own secret and is not checked.
            let server_side = multiset(&server, shadow.map(|c| (c.item, c.count)));
            let client_side = multiset(&client, cursor);
            for item in [spear, pale] {
                assert_eq!(
                    server_side.get(&item).copied().unwrap_or(0),
                    client_side.get(&item).copied().unwrap_or(0),
                    "step {step}: the server lost a {item:?}"
                );
            }
            // A disguised item on the client's cursor is mirrored in the shadow.
            if let Some((item, count)) = cursor {
                if t.wire(item).is_some_and(|w| w.disguised) {
                    assert_eq!(
                        shadow,
                        Some(Carried { item, count }),
                        "step {step}: shadow lost a cursor {item:?}"
                    );
                }
            }

            // With no conjures, a pure move conserves the whole *wire*
            // multiset — every item the client can see, count included. (The
            // real multiset is not conserved because a same-wire merge converts
            // a disguised stack into the slot's item, but the client cannot see
            // the difference, so the wire view must not change.)
            if !allow_creation {
                assert_eq!(
                    wire_multi(&client, cursor),
                    initial_wire,
                    "step {step}: a pure move changed the wire multiset"
                );
            }
        }
    }

    #[test]
    fn random_window_reports_never_mint_a_real_item() {
        // The property J10 must hold: random creative window actions never lose
        // or fabricate a *real* engine item. The server's slots always match
        // the client's, a disguised item is always accounted for, and with no
        // conjures the multiset is conserved exactly — count included, so a
        // loss fails too (N3), and conjure-onto-cursor exercises N4.
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

    /// A cursor shadow of `item` × `count`.
    fn held(item: ItemTypeId, count: u8) -> Option<Carried> {
        Some(Carried { item, count })
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
        let (res, carried) = t.resolve_change(None, Some((spear, 1)), None);
        assert_eq!(res, Resolution::Apply(None));
        assert_eq!(carried, held(spear, 1));

        // 2. Place into empty slot 10: the client reports the trident. The
        //    source slot is gone, so only the carried shadow identifies it.
        let (res, carried) = t.resolve_change(Some((shown, 1)), None, carried);
        assert_eq!(res, Resolution::Apply(Some((spear, 1))));
        assert_eq!(carried, None, "the one source is spent");

        // 3. Pick up and place again, into slot 11.
        let (_, carried) = t.resolve_change(None, Some((spear, 1)), carried);
        let (res, _) = t.resolve_change(Some((shown, 1)), None, carried);
        assert_eq!(res, Resolution::Apply(Some((spear, 1))));

        // 4. A trident with no source is a *creation*: a creative client can
        //    conjure one, so it is accepted as the genuine trident (F1).
        let (res, _) = t.resolve_change(Some((shown, 1)), None, None);
        assert_eq!(res, Resolution::Apply(Some((id("minecraft:trident"), 1))));
        // ...and a genuine item is always believed.
        let (res, _) = t.resolve_change(Some((id("minecraft:diamond"), 1)), None, None);
        assert_eq!(res, Resolution::Apply(Some((id("minecraft:diamond"), 1))));
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
        let (res, carried) = t.resolve_change(Some((shown, 1)), None, None);
        assert_eq!(
            res,
            Resolution::Apply(Some((id("minecraft:trident"), 1))),
            "no source at all: a creation"
        );
        assert_eq!(carried, None);

        // A real move: pick the spear up, place it, then try to place again.
        let (_, carried) = t.resolve_change(None, Some((spear, 1)), None);
        let (first, carried) = t.resolve_change(Some((shown, 1)), None, carried);
        assert_eq!(first, Resolution::Apply(Some((spear, 1))));
        assert_eq!(carried, None);
        let (second, _) = t.resolve_change(Some((shown, 1)), None, carried);
        assert_eq!(
            second,
            Resolution::Apply(Some((id("minecraft:trident"), 1))),
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
        let (_, carried) = t.resolve_change(None, Some((spear, 1)), None);
        assert_eq!(carried, held(spear, 1));
        // An empty report for a slot that was *already* empty says nothing
        // about the cursor, so the shadow must survive.
        let (res, carried) = t.resolve_change(None, None, carried);
        assert_eq!(res, Resolution::Apply(None));
        assert_eq!(carried, held(spear, 1), "the spear must not be lost");
        // And it can still be placed as the real item.
        let (res, _) = t.resolve_change(Some((shown, 1)), None, carried);
        assert_eq!(res, Resolution::Apply(Some((spear, 1))));
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
        let (res, _) = t.resolve_change(Some((shown, 1)), None, held(spear, 1));
        assert_eq!(res, Resolution::Apply(Some((spear, 1))));
        // A genuine item carried is preserved as itself, even with a spear
        // elsewhere in the window.
        let (res, _) = t.resolve_change(Some((shown, 1)), None, held(trident, 1));
        assert_eq!(res, Resolution::Apply(Some((trident, 1))));
        // A genuine trident already in the destination slot, with nothing on the
        // cursor, is the slot re-sending itself: the real item stays and no
        // cursor item is invented (the echo guard).
        let (res, carry) = t.resolve_change(Some((trident, 1)), Some((trident, 1)), None);
        assert_eq!(res, Resolution::Apply(Some((trident, 1))));
        assert_eq!(carry, None, "an echo does not invent a cursor item");
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
        let mut slot36 = Some((spear, 1));
        let mut slot10 = Some((trident, 1));
        let mut carried = None;

        // 2. Left-click slot 36: pick the spear up.
        let (res, carry) = t.resolve_change(None, slot36, carried);
        assert_eq!(res, Resolution::Apply(None));
        slot36 = None;
        carried = carry;
        assert_eq!(carried, held(spear, 1));

        // 3. Left-click slot 10: swap the spear onto the trident. The packet
        //    says "trident", which matches *both* slots.
        let (res, carry) = t.resolve_change(Some((shown, 1)), slot10, carried);
        match res {
            Resolution::Apply(i) => slot10 = i,
        }
        carried = carry;
        assert_eq!(slot10, Some((spear, 1)), "the spear must land in slot 10");
        assert_eq!(
            carried,
            held(trident, 1),
            "the trident is now on the cursor"
        );

        // No item is created or lost: exactly one spear and one trident.
        let all = [slot36, slot10, carried.map(|c| (c.item, c.count))];
        assert_eq!(all.iter().filter(|i| **i == Some((spear, 1))).count(), 1);
        assert_eq!(all.iter().filter(|i| **i == Some((trident, 1))).count(), 1);

        // A re-send matching the cursor's item merges rather than duplicates.
        let (res, carry) = t.resolve_change(Some((trident, 1)), Some((trident, 1)), carried);
        assert_eq!(res, Resolution::Apply(Some((trident, 1))));
        assert_eq!(carry, held(trident, 1));
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
            let (res, carry) = t.resolve_change(Some((picked, 1)), None, None);
            assert_eq!(res, Resolution::Apply(Some((picked, 1))), "{name} vanished");
            assert_eq!(carry, None);
        }
    }

    #[test]
    fn a_swap_of_an_unshadowed_cursor_item_onto_a_stand_in_keeps_the_stand_in() {
        // N4: the client clicks a creative-menu item onto its cursor (which
        // sends no packet) and then left-clicks a slot holding a spear shown as
        // a trident. The server never shadowed the cursor, so the report has no
        // source. The replaced spear must move to the cursor, not vanish and be
        // re-created as a genuine trident on the next placement.
        let spear = id("minecraft:iron_spear");
        let diamond = id("minecraft:diamond");
        let t = &V1_21_1;
        let shown = shown_as(t, spear).expect("the spear has a stand-in");

        // 1. Slot 9 holds the spear; the cursor is empty on the server.
        let mut slot9 = Some((spear, 1));
        // 2. Left-click slot 9: swap the conjured diamond onto the spear.
        let (res, carry) = t.resolve_change(Some((diamond, 1)), slot9, None);
        slot9 = match res {
            Resolution::Apply(s) => s,
        };
        assert_eq!(slot9, Some((diamond, 1)), "the diamond lands in the slot");
        assert_eq!(carry, held(spear, 1), "the spear goes to the cursor");

        // 3. Place the cursor item into an empty slot: it is still the spear,
        //    not a genuine trident.
        let (res, carry) = t.resolve_change(Some((shown, 1)), None, carry);
        assert_eq!(
            res,
            Resolution::Apply(Some((spear, 1))),
            "the spear is deposited, not conjured"
        );
        assert_eq!(carry, None);
    }

    #[test]
    fn a_partial_placement_spends_only_the_items_placed() {
        // N5: the shadow tracks the count. A right-click places one item from a
        // stackable stand-in — and `Set Creative Mode Slot` cannot say how many,
        // the packet has no button — so the rest must stay on the cursor as the
        // real item, not convert to the genuine stand-in.
        let pale = id("minecraft:pale_oak_planks");
        let t = &V1_21_1;
        let shown = shown_as(t, pale).expect("pale_oak_planks has a stand-in");
        assert_eq!(shown, id("minecraft:oak_planks"));

        // Pick up a stack of 64.
        let (_, carry) = t.resolve_change(None, Some((pale, 64)), None);
        assert_eq!(carry, held(pale, 64));
        // Place one into an empty slot.
        let (res, carry) = t.resolve_change(Some((shown, 1)), None, carry);
        assert_eq!(res, Resolution::Apply(Some((pale, 1))));
        assert_eq!(carry, held(pale, 63), "63 left, still the real item");
        // Place the rest.
        let (res, carry) = t.resolve_change(Some((shown, 63)), None, carry);
        assert_eq!(res, Resolution::Apply(Some((pale, 63))));
        assert_eq!(carry, None);
    }

    #[test]
    fn a_conjure_onto_a_non_empty_cursor_invalidates_the_shadow() {
        // Round 4: the server's model of the cursor can be stale — a creative
        // client puts a menu item on the cursor with no packet. A report the
        // shadow cannot explain must invalidate it and write the genuine item,
        // not materialise the engine-only item the client never had.
        let pale = id("minecraft:pale_oak_planks");
        let diamond = id("minecraft:diamond");
        let t = &V1_21_1;

        // The cursor is non-empty on the server: a picked-up stand-in.
        let (_, carry) = t.resolve_change(None, Some((pale, 1)), None);
        assert_eq!(carry, held(pale, 1));

        // The client replaces the cursor with a conjured diamond and places it.
        let (res, carry) = t.resolve_change(Some((diamond, 1)), None, carry);
        assert_eq!(res, Resolution::Apply(Some((diamond, 1))));
        assert_eq!(carry, None, "the stale shadow is invalidated, not trusted");
    }

    #[test]
    fn a_stand_in_wire_conjure_cannot_materialise_the_engine_item() {
        // Round 4: a genuine menu item whose wire id collides with a stand-in
        // (oak_planks, the stand-in for pale_oak_planks) conjured onto the
        // cursor and placed with a count the shadow never carried. The server
        // must write the genuine oak_planks — never the engine-only
        // pale_oak_planks, and never more of it than was carried.
        let pale = id("minecraft:pale_oak_planks");
        let oak = id("minecraft:oak_planks");
        let t = &V1_21_1;
        assert_eq!(shown_as(t, pale), Some(oak), "pale shows as oak");
        assert!(t.stand_in_wire(t.wire(oak).unwrap().id));

        // Pick one pale_oak_planks up.
        let (_, carry) = t.resolve_change(None, Some((pale, 1)), None);
        assert_eq!(carry, held(pale, 1));

        // The client conjures a full stack of genuine oak_planks onto the
        // cursor and places it into an empty slot. The count (64) is far more
        // than the shadow (1) carried, so the shadow cannot explain it.
        let (res, carry) = t.resolve_change(Some((oak, 64)), None, carry);
        assert_eq!(
            res,
            Resolution::Apply(Some((oak, 64))),
            "the genuine oak_planks, not 64 engine-only pale_oak_planks"
        );
        assert_eq!(carry, None);
    }

    #[test]
    fn a_disguised_stack_merging_onto_a_genuine_same_wire_stack_keeps_its_count() {
        // Round 5: the client cannot tell pale_oak_planks from oak_planks (same
        // wire id), so it right-click-merges a carried stack of 74
        // pale_oak_planks onto a genuine oak_planks stack of 11, filling the
        // slot to 64. The old merge/swap selection treated the different *real*
        // items as a swap and lost 53 of the 74 carried. The slot must keep its
        // authoritative item and consume exactly the added amount.
        let pale = id("minecraft:pale_oak_planks");
        let oak = id("minecraft:oak_planks");
        let t = &V1_21_1;
        assert_eq!(shown_as(t, pale), Some(oak), "pale shows as oak");

        // The cursor carries 74 pale_oak_planks (the shadow is the real item).
        let carried = held(pale, 74);
        // The slot holds 11 genuine oak_planks; the client reports oak × 64.
        let (res, carry) = t.resolve_change(Some((oak, 64)), Some((oak, 11)), carried);
        assert_eq!(
            res,
            Resolution::Apply(Some((oak, 64))),
            "the slot keeps its genuine oak_planks, filled to 64"
        );
        assert_eq!(
            carry,
            held(pale, 21),
            "exactly 53 of the 74 were consumed, 21 remain the real item"
        );
    }

    #[test]
    fn a_stackability_changing_stand_in_still_merges() {
        // Round 6: the client stacks by the item it is *shown*. A horse harness
        // (stack 1) is shown as leather (stack 64), so the client sees a
        // stackable leather and merges it onto a leather slot. Judging by the
        // *real* item's stack size (1) made the server swap, materialising 17
        // harnesses out of one and stranding the slot's leather.
        let harness = id("minecraft:white_harness");
        let leather = id("minecraft:leather");
        let t = &V1_21_1;
        assert_eq!(shown_as(t, harness), Some(leather));
        assert_eq!(crate::game::tables::max_stack_id(harness), 1, "real: 1");
        assert_eq!(crate::game::tables::max_stack_id(leather), 64, "shown: 64");

        // The cursor carries one harness; the slot holds 16 leather; the client
        // reports the merged leather × 17.
        let (res, carry) =
            t.resolve_change(Some((leather, 17)), Some((leather, 16)), held(harness, 1));
        assert_eq!(
            res,
            Resolution::Apply(Some((leather, 17))),
            "the slot keeps its leather, filled to 17"
        );
        assert_eq!(carry, None, "the one harness was merged in");
    }

    #[test]
    fn a_disguised_stack_merging_fully_onto_a_genuine_stack_leaves_nothing_carried() {
        // The smaller merge the old predicate rejected: 1 pale_oak_planks onto
        // 11 genuine oak_planks → 12, nothing left on the cursor.
        let pale = id("minecraft:pale_oak_planks");
        let oak = id("minecraft:oak_planks");
        let t = &V1_21_1;
        let (res, carry) = t.resolve_change(Some((oak, 12)), Some((oak, 11)), held(pale, 1));
        assert_eq!(res, Resolution::Apply(Some((oak, 12))));
        assert_eq!(carry, None, "the whole carried stack was merged in");
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
