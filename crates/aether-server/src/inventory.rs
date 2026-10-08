//! Player inventories: the whole 46 slots, tracked, persisted and restored.
//!
//! # Where the contents come from
//!
//! In creative the client fills its own slots from the item menu and reports
//! each one with Set Creative Mode Slot. That report is the *only* way the
//! server hears about an item it never handed out, so it is the source the
//! model is built from — see [`Inventory::set`].
//!
//! Consequence worth naming: this is not an authoritative inventory. A
//! creative client can claim to hold anything, and it is right to, because
//! creative means exactly that. When survival arrives the server will hand out
//! items and the client's report becomes a *check* rather than a source; the
//! shape here does not change, only who is believed.
//!
//! # Identity
//!
//! Every stack carries an [`ItemUid`], minted when the stack first appears and
//! carried through every move. That is what makes a duplication visible as one
//! uid in two places rather than as a suspicious total, and it is why this
//! module writes to the journal rather than only to a save file.
//!
//! # Persistence
//!
//! Inventories are written to the world's KV store — the same one the journal
//! and the sub-chunks share, under their own key prefix — on change and at
//! shutdown, and read back on join. The KV store rather than the SQL mirror on
//! purpose: a player must get their inventory back on a server whose database
//! is unreachable, and the mirror is derived from the journal in any case.

use aether_world::journal::{ledger::mint_uid, ItemUid};

/// Slots in a player's own window: 9 crafting/armour + 27 main + 9 hotbar +
/// the offhand.
pub const SLOTS: usize = 46;
/// Index of the first hotbar slot in the player's own window.
pub const FIRST_HOTBAR: usize = 36;
/// Index of the offhand slot.
pub const OFFHAND: usize = 45;
/// How many of one item a player may hold in one slot.
///
/// A rule about inventories, not about the protocol: since 1.20.5 a slot's
/// count travels as a VarInt and can say any number. See
/// [`crate::protocol::ContainerSlot::count`].
pub const STACK: u8 = 64;

/// One stack in one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    /// The version-neutral item name, e.g. `minecraft:diamond`.
    pub item: String,
    pub count: u8,
    /// This instance's identity, stable across every move it makes.
    pub uid: ItemUid,
    /// Wear on a tool or armour piece, `0` for new (and for anything that
    /// does not wear). The `minecraft:damage` data component on the wire.
    pub damage: u16,
}

impl Stack {
    /// A fresh stack with its own identity.
    pub fn new(item: &str, count: u8) -> Self {
        Self {
            item: item.to_owned(),
            count,
            uid: mint_uid(),
            damage: 0,
        }
    }

    /// The most of this item one slot may hold.
    pub fn max_stack(&self) -> u8 {
        crate::game::tables::max_stack(&self.item)
    }

    /// Whether `other` could merge into this stack: same item, same wear.
    pub fn stacks_with(&self, other: &Stack) -> bool {
        self.item == other.item && self.damage == other.damage && self.max_stack() > 1
    }

    /// Split `n` off this stack into a new instance, leaving the rest here.
    pub fn split(&mut self, n: u8) -> Stack {
        let n = n.min(self.count);
        self.count -= n;
        Stack {
            item: self.item.clone(),
            count: n,
            uid: mint_uid(),
            damage: self.damage,
        }
    }
}

/// A player's inventory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inventory {
    slots: Vec<Option<Stack>>,
    /// Selected hotbar slot, `0..9`.
    held: u8,
}

impl Inventory {
    /// An empty inventory.
    pub fn new() -> Self {
        Self {
            slots: vec![None; SLOTS],
            held: 0,
        }
    }

    fn ensure(&mut self) {
        if self.slots.len() != SLOTS {
            self.slots.resize(SLOTS, None);
        }
    }

    /// The stack in `slot`, if any.
    pub fn get(&self, slot: usize) -> Option<&Stack> {
        self.slots.get(slot).and_then(|s| s.as_ref())
    }

    /// Replace `slot`'s contents, minting a uid for a stack that is new.
    ///
    /// Returns the uid now in the slot, and the uid that left it — the two
    /// halves of the journal entries the caller writes.
    ///
    /// A report that names the same item and count as what is already there
    /// keeps the existing uid rather than minting a fresh one. Clients resend
    /// slot contents freely (after a window close, a respawn, a resync), and
    /// minting on every resend would fill the ledger with thousands of
    /// one-hop instances and bury the real duplications.
    pub fn set(&mut self, slot: usize, item: Option<(&str, u8)>) -> SlotChange {
        self.ensure();
        if slot >= SLOTS {
            return SlotChange::default();
        }
        let before = self.slots[slot].clone();
        let after = match item {
            None => None,
            Some((_, 0)) => None,
            Some((name, count)) => match &before {
                Some(s) if s.item == name && s.count == count => Some(s.clone()),
                _ => Some(Stack::new(name, count)),
            },
        };
        let unchanged = before == after;
        self.slots[slot] = after.clone();
        SlotChange {
            removed: if unchanged { None } else { before },
            added: if unchanged { None } else { after },
        }
    }

    /// Record the selected hotbar slot.
    pub fn select(&mut self, held: u8) {
        if held < 9 {
            self.held = held;
        }
    }

    /// The selected hotbar slot, `0..9`.
    pub fn held_slot(&self) -> u8 {
        self.held
    }

    /// The stack the player is holding.
    pub fn held(&self) -> Option<&Stack> {
        self.get(FIRST_HOTBAR + self.held as usize)
    }

    /// Add `count` of `item` to the first slot that will take it, preferring
    /// the hotbar. Returns the slot used, or `None` when the inventory is
    /// full.
    ///
    /// Never merges into an existing stack: merging would have to decide which
    /// of the two uids survives, and either answer loses a trail the ledger
    /// depends on. A separate slot keeps both identities intact.
    pub fn give(&mut self, item: &str, count: u8) -> Option<usize> {
        self.ensure();
        let order = (FIRST_HOTBAR..OFFHAND).chain(9..FIRST_HOTBAR);
        let slot = order.into_iter().find(|s| self.slots[*s].is_none())?;
        self.slots[slot] = Some(Stack::new(item, count));
        Some(slot)
    }

    /// Raw access to one slot, for the window logic that moves stacks
    /// around. Out-of-range indices read as empty.
    pub fn slot(&self, slot: usize) -> Option<&Stack> {
        self.get(slot)
    }

    /// Take whatever is in `slot`, leaving it empty.
    pub fn take_slot(&mut self, slot: usize) -> Option<Stack> {
        self.ensure();
        self.slots.get_mut(slot).and_then(Option::take)
    }

    /// Put `stack` into `slot`, returning what was there.
    pub fn put_slot(&mut self, slot: usize, stack: Option<Stack>) -> Option<Stack> {
        self.ensure();
        match self.slots.get_mut(slot) {
            Some(s) => std::mem::replace(s, stack.filter(|s| s.count > 0)),
            None => stack,
        }
    }

    /// Mutable access to one slot.
    pub fn slot_mut(&mut self, slot: usize) -> Option<&mut Option<Stack>> {
        self.ensure();
        self.slots.get_mut(slot)
    }

    /// Merge `stack` into the inventory: topping up matching stacks first,
    /// then empty slots, hotbar first as vanilla does. Returns what did not
    /// fit.
    ///
    /// Merging keeps the *destination's* uid and the arriving instance's uid
    /// ends there. That loses one hop of provenance per merge, which is the
    /// price of stacks behaving the way every player expects them to.
    pub fn insert(&mut self, mut stack: Stack) -> Option<Stack> {
        self.ensure();
        let order: Vec<usize> = (FIRST_HOTBAR..OFFHAND).chain(9..FIRST_HOTBAR).collect();
        let max = stack.max_stack();
        for &i in &order {
            if stack.count == 0 {
                return None;
            }
            if let Some(s) = &mut self.slots[i] {
                if s.stacks_with(&stack) && s.count < max {
                    let n = (max - s.count).min(stack.count);
                    s.count += n;
                    stack.count -= n;
                }
            }
        }
        for &i in &order {
            if stack.count == 0 {
                return None;
            }
            if self.slots[i].is_none() {
                let n = stack.count.min(max);
                self.slots[i] = Some(stack.split(n));
            }
        }
        (stack.count > 0).then_some(stack)
    }

    /// The selected hotbar slot's window index.
    pub fn held_index(&self) -> usize {
        FIRST_HOTBAR + self.held as usize
    }

    /// Remove one item from the held stack. Returns it.
    pub fn consume_held(&mut self) -> Option<Stack> {
        let i = self.held_index();
        let slot = self.slot_mut(i)?;
        let s = slot.as_mut()?;
        let one = s.split(1);
        if s.count == 0 {
            *slot = None;
        }
        Some(one)
    }

    /// Add `count` of `item`, splitting it across as many slots as it takes.
    /// Returns what would not fit.
    ///
    /// Sixty-four per slot, because that is the limit a *player* is subject
    /// to — the wire can carry more, and the recovery manifest does, but an
    /// inventory that hands out a slot of 4000 is one the client will
    /// disagree with the moment it is touched.
    ///
    /// The caller is expected to do something with the remainder. Dropping it
    /// on the floor is not a detail this function should decide, but silently
    /// losing it is not an option either, which is why the count comes back
    /// rather than being discarded here.
    pub fn give_many(&mut self, item: &str, count: u64) -> u64 {
        let mut left = count;
        let max = crate::game::tables::max_stack(item).min(STACK) as u64;
        while left > 0 {
            let chunk = left.min(max) as u8;
            match self.insert(Stack::new(item, chunk)) {
                None => left -= chunk as u64,
                Some(rest) => {
                    left -= (chunk - rest.count) as u64;
                    break;
                }
            }
        }
        left
    }

    /// Remove `count` of `item`, across as many slots as it takes. Returns how
    /// many were actually removed.
    pub fn take(&mut self, item: &str, mut count: u64) -> u64 {
        self.ensure();
        let mut taken = 0;
        for slot in self.slots.iter_mut() {
            if count == 0 {
                break;
            }
            let Some(s) = slot else { continue };
            if s.item != item {
                continue;
            }
            let n = (s.count as u64).min(count);
            s.count -= n as u8;
            count -= n;
            taken += n;
            if s.count == 0 {
                *slot = None;
            }
        }
        taken
    }

    /// How many of `item` are held, across every slot.
    pub fn count_of(&self, item: &str) -> u64 {
        self.slots
            .iter()
            .flatten()
            .filter(|s| s.item == item)
            .map(|s| s.count as u64)
            .sum()
    }

    /// Every occupied slot, in slot order.
    pub fn occupied(&self) -> impl Iterator<Item = (usize, &Stack)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|s| (i, s)))
    }

    /// Whether nothing is held at all.
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }
}

/// What one [`Inventory::set`] changed, so the caller can journal it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SlotChange {
    /// The stack that left the slot.
    pub removed: Option<Stack>,
    /// The stack that arrived.
    pub added: Option<Stack>,
}

impl SlotChange {
    /// Whether anything actually changed.
    pub fn is_noop(&self) -> bool {
        self.removed.is_none() && self.added.is_none()
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Bumped whenever the layout below changes. A blob that does not start with
/// this is refused rather than guessed at: a misread inventory would hand a
/// player somebody else's items.
pub const FORMAT_VERSION: u8 = 2;

/// Serialize for the KV store.
pub fn encode(inv: &Inventory) -> Vec<u8> {
    let mut out = vec![FORMAT_VERSION, inv.held];
    let occupied: Vec<(usize, &Stack)> = inv.occupied().collect();
    out.extend_from_slice(&(occupied.len() as u16).to_be_bytes());
    for (slot, s) in occupied {
        out.push(slot as u8);
        out.push(s.count);
        out.extend_from_slice(&s.uid.0.to_be_bytes());
        out.extend_from_slice(&s.damage.to_be_bytes());
        out.extend_from_slice(&(s.item.len() as u16).to_be_bytes());
        out.extend_from_slice(s.item.as_bytes());
    }
    out
}

/// Inverse of [`encode`]. `None` for anything unreadable — an unreadable
/// inventory reads as an empty one, never as a partial one, because handing a
/// player half their items is worse than handing them none and saying so.
pub fn decode(blob: &[u8]) -> Option<Inventory> {
    let mut c = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let end = c.checked_add(n)?;
        let s = blob.get(c..end)?;
        c = end;
        Some(s)
    };
    // Version 1 is version 2 without the wear field.
    let version = take(1)?[0];
    if version != FORMAT_VERSION && version != 1 {
        return None;
    }
    let held = take(1)?[0];
    let n = u16::from_be_bytes(take(2)?.try_into().ok()?) as usize;
    let mut inv = Inventory::new();
    inv.held = held.min(8);
    for _ in 0..n {
        let slot = take(1)?[0] as usize;
        let count = take(1)?[0];
        let uid = u128::from_be_bytes(take(16)?.try_into().ok()?);
        let damage = if version >= 2 {
            u16::from_be_bytes(take(2)?.try_into().ok()?)
        } else {
            0
        };
        let len = u16::from_be_bytes(take(2)?.try_into().ok()?) as usize;
        let item = std::str::from_utf8(take(len)?).ok()?.to_owned();
        if slot < SLOTS && count > 0 {
            inv.slots[slot] = Some(Stack {
                item,
                count,
                uid: ItemUid(uid),
                damage,
            });
        }
    }
    Some(inv)
}

/// KV key holding one player's inventory.
///
/// Seventeen bytes, where sub-chunk keys are nine, column markers ten and
/// journal event keys nine, so it can collide with none of them.
pub fn key(uuid: u128) -> [u8; 17] {
    let mut k = [0u8; 17];
    k[0] = b'V';
    k[1..].copy_from_slice(&uuid.to_be_bytes());
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_inventory_is_empty_and_holds_the_first_hotbar_slot() {
        let inv = Inventory::new();
        assert!(inv.is_empty());
        assert_eq!(inv.held_slot(), 0);
        assert!(inv.held().is_none());
    }

    #[test]
    fn every_stack_gets_its_own_identity() {
        let mut inv = Inventory::new();
        inv.set(36, Some(("minecraft:stone", 64)));
        inv.set(37, Some(("minecraft:stone", 64)));
        let a = inv.get(36).unwrap().uid;
        let b = inv.get(37).unwrap().uid;
        assert_ne!(a, b, "two stacks of the same item are two instances");
    }

    #[test]
    fn re_reporting_the_same_slot_keeps_the_uid() {
        // Clients resend slot contents freely. Minting on every resend would
        // bury real duplications under thousands of one-hop instances.
        let mut inv = Inventory::new();
        inv.set(36, Some(("minecraft:stone", 64)));
        let first = inv.get(36).unwrap().uid;
        let change = inv.set(36, Some(("minecraft:stone", 64)));
        assert!(change.is_noop(), "an identical report is not a change");
        assert_eq!(inv.get(36).unwrap().uid, first);
    }

    #[test]
    fn changing_the_count_is_a_new_instance() {
        // A different count is a different stack, and pretending otherwise
        // would let a client grow a stack in place with no event to show for
        // it — precisely the shape of a duplication.
        let mut inv = Inventory::new();
        inv.set(36, Some(("minecraft:stone", 1)));
        let first = inv.get(36).unwrap().uid;
        let change = inv.set(36, Some(("minecraft:stone", 64)));
        assert!(!change.is_noop());
        assert_eq!(change.removed.unwrap().uid, first);
        assert_ne!(change.added.unwrap().uid, first);
    }

    #[test]
    fn clearing_a_slot_reports_what_left_it() {
        let mut inv = Inventory::new();
        inv.set(36, Some(("minecraft:diamond", 3)));
        let change = inv.set(36, None);
        assert_eq!(change.removed.unwrap().item, "minecraft:diamond");
        assert!(change.added.is_none());
        assert!(inv.is_empty());
    }

    #[test]
    fn a_zero_count_is_an_empty_slot_not_a_stack_of_nothing() {
        let mut inv = Inventory::new();
        inv.set(36, Some(("minecraft:stone", 0)));
        assert!(inv.get(36).is_none());
    }

    #[test]
    fn a_slot_outside_the_window_is_ignored_rather_than_panicking() {
        // The slot index comes off the wire, so it is whatever a client says.
        let mut inv = Inventory::new();
        assert!(inv.set(9999, Some(("minecraft:stone", 1))).is_noop());
        assert!(inv.is_empty());
    }

    #[test]
    fn the_held_stack_follows_the_selected_slot() {
        let mut inv = Inventory::new();
        inv.set(FIRST_HOTBAR + 3, Some(("minecraft:torch", 12)));
        assert!(inv.held().is_none());
        inv.select(3);
        assert_eq!(inv.held().unwrap().item, "minecraft:torch");
    }

    #[test]
    fn selecting_a_slot_outside_the_hotbar_is_ignored() {
        let mut inv = Inventory::new();
        inv.select(200);
        assert_eq!(inv.held_slot(), 0);
    }

    #[test]
    fn giving_fills_the_hotbar_before_the_backpack() {
        let mut inv = Inventory::new();
        assert_eq!(inv.give("minecraft:stone", 1), Some(FIRST_HOTBAR));
        for _ in 1..9 {
            inv.give("minecraft:stone", 1);
        }
        // Hotbar full: the next one goes to the main inventory, not the
        // offhand.
        let next = inv.give("minecraft:stone", 1).unwrap();
        assert!((9..FIRST_HOTBAR).contains(&next), "went to slot {next}");
    }

    #[test]
    fn a_large_gift_is_split_into_stacks() {
        let mut inv = Inventory::new();
        assert_eq!(inv.give_many("minecraft:stone", 200), 0, "all of it fitted");
        assert_eq!(inv.count_of("minecraft:stone"), 200);
        let counts: Vec<u8> = inv.occupied().map(|(_, s)| s.count).collect();
        assert_eq!(
            counts,
            vec![64, 64, 64, 8],
            "full stacks, then the remainder"
        );
    }

    #[test]
    fn a_gift_that_does_not_fit_reports_the_remainder_instead_of_losing_it() {
        // The number that comes back is what ends up on the floor. Returning
        // nothing here would make a full inventory delete items silently.
        let mut inv = Inventory::new();
        let free = (FIRST_HOTBAR..OFFHAND).len() + (9..FIRST_HOTBAR).len();
        let capacity = free as u64 * STACK as u64;
        assert_eq!(inv.give_many("minecraft:stone", capacity + 100), 100);
        assert_eq!(inv.count_of("minecraft:stone"), capacity);
    }

    #[test]
    fn giving_exactly_a_stack_uses_exactly_one_slot() {
        let mut inv = Inventory::new();
        assert_eq!(inv.give_many("minecraft:stone", 64), 0);
        assert_eq!(inv.occupied().count(), 1);
    }

    #[test]
    fn giving_into_a_full_inventory_fails_rather_than_overwriting() {
        let mut inv = Inventory::new();
        while inv.give("minecraft:stone", 1).is_some() {}
        assert_eq!(inv.give("minecraft:diamond", 1), None);
        assert_eq!(inv.count_of("minecraft:diamond"), 0);
    }

    #[test]
    fn taking_spans_slots_and_reports_a_short_fall() {
        let mut inv = Inventory::new();
        inv.set(36, Some(("minecraft:stone", 30)));
        inv.set(37, Some(("minecraft:stone", 30)));
        assert_eq!(inv.count_of("minecraft:stone"), 60);
        assert_eq!(inv.take("minecraft:stone", 45), 45);
        assert_eq!(inv.count_of("minecraft:stone"), 15);
        // Asking for more than is there takes what there is and says so,
        // rather than failing and leaving the caller to guess.
        assert_eq!(inv.take("minecraft:stone", 100), 15);
        assert!(inv.is_empty());
    }

    #[test]
    fn taking_leaves_other_items_alone() {
        let mut inv = Inventory::new();
        inv.set(36, Some(("minecraft:stone", 10)));
        inv.set(37, Some(("minecraft:diamond", 10)));
        inv.take("minecraft:stone", 100);
        assert_eq!(inv.count_of("minecraft:diamond"), 10);
    }

    #[test]
    fn an_inventory_survives_a_round_trip_with_its_identities_intact() {
        let mut inv = Inventory::new();
        inv.set(0, Some(("minecraft:crafting_table", 1)));
        inv.set(FIRST_HOTBAR, Some(("minecraft:stone", 64)));
        inv.set(OFFHAND, Some(("minecraft:shield", 1)));
        inv.select(5);
        let back = decode(&encode(&inv)).expect("decodes");
        assert_eq!(back, inv, "uids and slots must both survive");
        assert_eq!(back.held_slot(), 5);
    }

    #[test]
    fn an_empty_inventory_round_trips() {
        let inv = Inventory::new();
        assert_eq!(decode(&encode(&inv)), Some(inv));
    }

    #[test]
    fn a_blob_from_another_version_is_refused() {
        let mut b = encode(&Inventory::new());
        b[0] = FORMAT_VERSION.wrapping_add(1);
        assert_eq!(decode(&b), None);
        b[0] = 0;
        assert_eq!(decode(&b), None);
    }

    #[test]
    fn a_truncated_blob_is_refused_at_every_length() {
        // Never a partial inventory: handing a player half their items is
        // worse than handing them none and saying so.
        let mut inv = Inventory::new();
        inv.set(36, Some(("minecraft:stone", 64)));
        inv.set(37, Some(("minecraft:diamond_pickaxe", 1)));
        let full = encode(&inv);
        for n in 0..full.len() {
            assert!(decode(&full[..n]).is_none(), "prefix of {n} bytes decoded");
        }
        assert!(decode(&full).is_some());
    }

    #[test]
    fn the_key_cannot_collide_with_a_sub_chunk_or_a_journal_entry() {
        let k = key(1);
        assert_eq!(k.len(), 17);
        assert_eq!(k[0], b'V');
        // Journal event keys are 9 bytes and start with 'E'; column indexes
        // are 17 bytes but start with 'X'; actor indexes are 25.
        assert_ne!(k[0], b'E');
        assert_ne!(k[0], b'X');
        assert_ne!(k[0], b'C');
    }
}
