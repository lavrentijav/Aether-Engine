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
use aether_world::ItemTypeId;

/// Resolve a canonical item name to its id, for a name the game's own tables
/// hold. An unknown name is a typo in a literal, so it is a bug and panics
/// ([`ItemTypeId::vanilla`] is the non-panicking form).
pub fn item(name: &str) -> ItemTypeId {
    ItemTypeId::vanilla(name).unwrap_or_else(|| panic!("{name} is not an item in this release"))
}

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
/// Client-side presentation overrides for a **virtual** menu item.
///
/// Real stacks a player carries never have one; a plugin menu draws its
/// buttons with a name and lore the real item does not carry. These are
/// rendering hints only — they are not persisted (see [`encode`]) and never
/// affect what an item *is*: a menu item is a stand-in for an action, not an
/// item the player owns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemDisplay {
    /// The `minecraft:custom_name` text, if any.
    pub name: Option<String>,
    /// `minecraft:lore` lines, in order.
    pub lore: Vec<String>,
    /// The `minecraft:enchantment_glint_override` flag.
    pub glint: bool,
    /// The `minecraft:custom_model_data` id.
    pub model: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    /// The item type, an index into the world's item registry — two bytes
    /// rather than a heap string, and compared by id.
    pub item: ItemTypeId,
    pub count: u8,
    /// This instance's identity, stable across every move it makes.
    pub uid: ItemUid,
    /// Wear on a tool or armour piece, `0` for new (and for anything that
    /// does not wear). The `minecraft:damage` data component on the wire.
    pub damage: u16,
    /// The `minecraft:potion` registry index this stack holds, for a potion,
    /// splash potion or lingering potion. `None` is everything else — the
    /// `minecraft:potion_contents` data component on the wire, which is what
    /// tells the client which potion it is.
    pub potion: Option<u16>,
    /// Display overrides for a plugin menu item; `None` for a real stack.
    pub display: Option<Box<ItemDisplay>>,
}

impl Stack {
    /// A fresh stack of `item` with its own identity.
    pub fn new(item: ItemTypeId, count: u8) -> Self {
        Self {
            item,
            count,
            uid: mint_uid(),
            damage: 0,
            potion: None,
            display: None,
        }
    }

    /// A stack presented with a name, lore, glint and model id, for a plugin
    /// menu item. The stack itself is real (it decides the icon); the display
    /// is what the client draws. Never entered into a real inventory.
    pub fn menu_item(
        item: ItemTypeId,
        count: u8,
        name: Option<String>,
        lore: Vec<String>,
        glint: bool,
        model: Option<i32>,
    ) -> Self {
        let mut s = Self::new(item, count);
        s.display = Some(Box::new(ItemDisplay {
            name,
            lore,
            glint,
            model,
        }));
        s
    }

    /// Whether this stack is a display-only menu item.
    pub fn is_menu_item(&self) -> bool {
        self.display.is_some()
    }

    /// [`Stack::new`] from a canonical item name, for the game's own literals.
    /// Panics on an unknown name: a typo is a bug.
    pub fn named(item: &str, count: u8) -> Self {
        Self::new(crate::inventory::item(item), count)
    }

    /// A potion, splash potion or lingering potion of `potion` (a
    /// `minecraft:potion` registry index), from a canonical item name.
    pub fn potion(item: &str, potion: u16, count: u8) -> Self {
        Self {
            potion: Some(potion),
            ..Self::named(item, count)
        }
    }

    /// The `minecraft:potion` registry index, or `None` for everything else.
    pub fn potion_id(&self) -> Option<u16> {
        self.potion
    }

    /// The most of this item one slot may hold.
    pub fn max_stack(&self) -> u8 {
        crate::game::tables::max_stack_id(self.item)
    }

    /// Whether `other` could merge into this stack: same item, same wear, and
    /// the same potion contents. Two different potions never merge.
    pub fn stacks_with(&self, other: &Stack) -> bool {
        self.item == other.item
            && self.damage == other.damage
            && self.potion == other.potion
            && self.max_stack() > 1
    }

    /// Split `n` off this stack into a new instance, leaving the rest here.
    pub fn split(&mut self, n: u8) -> Stack {
        let n = n.min(self.count);
        self.count -= n;
        Stack {
            item: self.item,
            count: n,
            uid: mint_uid(),
            damage: self.damage,
            potion: self.potion,
            display: self.display.clone(),
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
    pub fn set(&mut self, slot: usize, item: Option<(ItemTypeId, u8)>) -> SlotChange {
        self.ensure();
        if slot >= SLOTS {
            return SlotChange::default();
        }
        let before = self.slots[slot].clone();
        let after = match item {
            None => None,
            Some((_, 0)) => None,
            Some((item, count)) => match &before {
                Some(s) if s.item == item && s.count == count => Some(s.clone()),
                _ => Some(Stack::new(item, count)),
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
    pub fn give(&mut self, item: ItemTypeId, count: u8) -> Option<usize> {
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
                if stack.count <= max {
                    // The whole remainder fits in one slot, so it keeps its
                    // identity: a pickup into an empty slot is a move of the
                    // same instance, not a mint of a copy.
                    self.slots[i] = Some(stack);
                    return None;
                }
                self.slots[i] = Some(stack.split(max));
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
        self.consume_at(self.held_index())
    }

    /// Remove one item from `slot`. Returns it, or `None` when the slot was
    /// already empty.
    pub fn consume_at(&mut self, slot: usize) -> Option<Stack> {
        self.ensure();
        let cell = self.slots.get_mut(slot)?;
        let s = cell.as_mut()?;
        let one = s.split(1);
        if s.count == 0 {
            *cell = None;
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
    pub fn give_many(&mut self, item: ItemTypeId, count: u64) -> u64 {
        let mut left = count;
        let max = crate::game::tables::max_stack_id(item).min(STACK) as u64;
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

    /// [`Inventory::give_many`], but reporting where each unit went so the
    /// caller can journal it: a [`GiveStep::Placed`] is a fresh stack that
    /// went to an empty slot, a [`GiveStep::Merged`] says which existing stack
    /// a fresh uid's units topped up. Returns the steps and what did not fit.
    pub fn give_many_report(&mut self, item: ItemTypeId, count: u64) -> (Vec<GiveStep>, u64) {
        self.ensure();
        let mut steps = Vec::new();
        let mut left = count;
        let max = crate::game::tables::max_stack_id(item).min(STACK);
        let order: Vec<usize> = (FIRST_HOTBAR..OFFHAND).chain(9..FIRST_HOTBAR).collect();
        let stackable = crate::game::tables::max_stack_id(item) > 1;
        while left > 0 {
            let chunk = left.min(max as u64) as u8;
            let mut remaining = chunk;
            if stackable {
                for &i in &order {
                    if remaining == 0 {
                        break;
                    }
                    if let Some(s) = &mut self.slots[i] {
                        if s.item == item && s.count < max {
                            let n = (max - s.count).min(remaining);
                            s.count += n;
                            remaining -= n;
                            steps.push(GiveStep::Merged {
                                slot: i,
                                into: s.uid,
                                mint: mint_uid(),
                                count: n,
                            });
                        }
                    }
                }
            }
            for &i in &order {
                if remaining == 0 {
                    break;
                }
                if self.slots[i].is_none() {
                    let n = remaining.min(max);
                    remaining -= n;
                    let st = Stack::new(item, n);
                    steps.push(GiveStep::Placed {
                        slot: i,
                        stack: st.clone(),
                    });
                    self.slots[i] = Some(st);
                }
            }
            let moved = (chunk - remaining) as u64;
            if moved == 0 {
                break;
            }
            left -= moved;
        }
        (steps, left)
    }

    /// Remove `count` of `item`, across as many slots as it takes. Returns how
    /// many were actually removed.
    pub fn take(&mut self, item: ItemTypeId, mut count: u64) -> u64 {
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
    pub fn count_of(&self, item: ItemTypeId) -> u64 {
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

/// Where one gift's units went, so the caller can journal a mint (and, for a
/// top-up, the merge that followed) without re-deriving it.
#[derive(Debug, Clone, PartialEq)]
pub enum GiveStep {
    /// A fresh stack placed into an empty slot.
    Placed { slot: usize, stack: Stack },
    /// `count` fresh units (uid `mint`) merged into the stack at `slot`.
    Merged {
        slot: usize,
        into: ItemUid,
        mint: ItemUid,
        count: u8,
    },
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
///
/// Version 5 added the potion contents field; version 4 stores the item as a
/// two-byte id rather than a length-prefixed name; version 3 narrowed item uids
/// from 16 bytes to 8 (see [`aether_world::journal::event::ItemUid`]); version
/// 2 added the wear field. A version 4 blob loads, its stacks holding no
/// potion. A version 3 blob loads too: its only difference is the item stored
/// as a name, which resolves to the same vanilla id losslessly (an unknown name
/// refuses the whole inventory). Versions 1–2 carry 16-byte uids that have no
/// 8-byte equivalent, so they are refused — handing a player the wrong items is
/// worse than handing them none — unless they hold no stack at all: an empty
/// inventory has nothing to misread, and refusing it locked players out.
pub const FORMAT_VERSION: u8 = 5;

/// A potion field's "no potion" sentinel in the version 5 layout.
const NO_POTION: u16 = u16::MAX;

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
        out.extend_from_slice(&s.item.0.to_be_bytes());
        out.extend_from_slice(&s.potion.unwrap_or(NO_POTION).to_be_bytes());
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
    let version = take(1)?[0];
    if !(1..=FORMAT_VERSION).contains(&version) {
        return None;
    }
    let held = take(1)?[0];
    let n = u16::from_be_bytes(take(2)?.try_into().ok()?) as usize;
    let mut inv = Inventory::new();
    inv.held = held.min(8);
    if version <= 2 {
        // 16-byte uids: only an empty inventory is unambiguous.
        return (n == 0 && c == blob.len()).then_some(inv);
    }
    for _ in 0..n {
        let slot = take(1)?[0] as usize;
        let count = take(1)?[0];
        let uid = u64::from_be_bytes(take(8)?.try_into().ok()?);
        let damage = u16::from_be_bytes(take(2)?.try_into().ok()?);
        let item = if version == 3 {
            let len = u16::from_be_bytes(take(2)?.try_into().ok()?) as usize;
            ItemTypeId::vanilla(std::str::from_utf8(take(len)?).ok()?)?
        } else {
            ItemTypeId(u16::from_be_bytes(take(2)?.try_into().ok()?))
        };
        // Version 4 predates the potion field.
        let potion = if version >= 5 {
            match u16::from_be_bytes(take(2)?.try_into().ok()?) {
                NO_POTION => None,
                p => Some(p),
            }
        } else {
            None
        };
        if slot < SLOTS && count > 0 {
            inv.slots[slot] = Some(Stack {
                item,
                count,
                uid: ItemUid(uid),
                damage,
                potion,
                // Display overrides are a menu's, never a saved inventory's:
                // a persisted stack is something the player really holds.
                display: None,
            });
        }
    }
    Some(inv)
}

/// KV key holding one player's inventory.
///
/// Seventeen bytes under the reserved `F` prefix (see [`aether_api::keys`]).
/// It used to be `V`, which the storage layer's snapshot-revision key also
/// claims; the two never had the same length so they did not collide in
/// practice, but one prefix byte for two owners is the sort of thing the
/// key-space test exists to stop. [`legacy_key`] reads the old key so a
/// returning player keeps their inventory across the rename.
pub fn key(uuid: u128) -> [u8; 17] {
    let mut k = [0u8; 17];
    k[0] = aether_api::keys::PLAYER_INVENTORY[0];
    k[1..].copy_from_slice(&uuid.to_be_bytes());
    k
}

/// The pre-rename key (`V`). Read fallback only; nothing is ever written here.
pub fn legacy_key(uuid: u128) -> [u8; 17] {
    let mut k = [0u8; 17];
    k[0] = b'V';
    k[1..].copy_from_slice(&uuid.to_be_bytes());
    k
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_old_empty_inventory_loads_as_empty_rather_than_locking_the_player_out() {
        for version in 1..=3u8 {
            let inv = decode(&[version, 2, 0, 0])
                .expect("an empty v1-3 inventory has nothing to misread");
            assert_eq!(inv.occupied().count(), 0, "v{version}");
            assert_eq!(inv.held, 2);
        }
        // Trailing bytes after an "empty" v1/v2 header are not empty at all.
        assert!(decode(&[2, 0, 0, 0, 9]).is_none());
    }

    #[test]
    fn a_version_3_inventory_resolves_its_item_names() {
        let name = b"minecraft:diamond";
        let mut b = vec![3u8, 0, 0, 1, 5, 7];
        b.extend_from_slice(&42u64.to_be_bytes());
        b.extend_from_slice(&3u16.to_be_bytes());
        b.extend_from_slice(&(name.len() as u16).to_be_bytes());
        b.extend_from_slice(name);
        let inv = decode(&b).expect("v3 decodes");
        let s = inv.slot(5).expect("slot 5");
        assert_eq!(s.item, item("minecraft:diamond"));
        assert_eq!((s.count, s.uid.0, s.damage, s.potion), (7, 42, 3, None));

        // An unknown name refuses the whole inventory rather than dropping it.
        let mut bad = b.clone();
        let at = bad.len() - name.len();
        bad[at..].copy_from_slice(b"minecraft:diamonX");
        assert!(decode(&bad).is_none());
    }

    #[test]
    fn a_non_empty_version_2_inventory_is_still_refused() {
        assert!(decode(&[2, 0, 0, 1, 0, 1]).is_none());
    }

    use super::*;

    /// A canonical item id by name, for the tests' literals.
    fn it(name: &str) -> ItemTypeId {
        item(name)
    }

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
        inv.set(36, Some((it("minecraft:stone"), 64)));
        inv.set(37, Some((it("minecraft:stone"), 64)));
        let a = inv.get(36).unwrap().uid;
        let b = inv.get(37).unwrap().uid;
        assert_ne!(a, b, "two stacks of the same item are two instances");
    }

    #[test]
    fn re_reporting_the_same_slot_keeps_the_uid() {
        // Clients resend slot contents freely. Minting on every resend would
        // bury real duplications under thousands of one-hop instances.
        let mut inv = Inventory::new();
        inv.set(36, Some((it("minecraft:stone"), 64)));
        let first = inv.get(36).unwrap().uid;
        let change = inv.set(36, Some((it("minecraft:stone"), 64)));
        assert!(change.is_noop(), "an identical report is not a change");
        assert_eq!(inv.get(36).unwrap().uid, first);
    }

    #[test]
    fn changing_the_count_is_a_new_instance() {
        // A different count is a different stack, and pretending otherwise
        // would let a client grow a stack in place with no event to show for
        // it — precisely the shape of a duplication.
        let mut inv = Inventory::new();
        inv.set(36, Some((it("minecraft:stone"), 1)));
        let first = inv.get(36).unwrap().uid;
        let change = inv.set(36, Some((it("minecraft:stone"), 64)));
        assert!(!change.is_noop());
        assert_eq!(change.removed.unwrap().uid, first);
        assert_ne!(change.added.unwrap().uid, first);
    }

    #[test]
    fn clearing_a_slot_reports_what_left_it() {
        let mut inv = Inventory::new();
        inv.set(36, Some((it("minecraft:diamond"), 3)));
        let change = inv.set(36, None);
        assert_eq!(change.removed.unwrap().item, it("minecraft:diamond"));
        assert!(change.added.is_none());
        assert!(inv.is_empty());
    }

    #[test]
    fn a_zero_count_is_an_empty_slot_not_a_stack_of_nothing() {
        let mut inv = Inventory::new();
        inv.set(36, Some((it("minecraft:stone"), 0)));
        assert!(inv.get(36).is_none());
    }

    #[test]
    fn a_slot_outside_the_window_is_ignored_rather_than_panicking() {
        // The slot index comes off the wire, so it is whatever a client says.
        let mut inv = Inventory::new();
        assert!(inv.set(9999, Some((it("minecraft:stone"), 1))).is_noop());
        assert!(inv.is_empty());
    }

    #[test]
    fn the_held_stack_follows_the_selected_slot() {
        let mut inv = Inventory::new();
        inv.set(FIRST_HOTBAR + 3, Some((it("minecraft:torch"), 12)));
        assert!(inv.held().is_none());
        inv.select(3);
        assert_eq!(inv.held().unwrap().item, it("minecraft:torch"));
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
        assert_eq!(inv.give(it("minecraft:stone"), 1), Some(FIRST_HOTBAR));
        for _ in 1..9 {
            inv.give(it("minecraft:stone"), 1);
        }
        // Hotbar full: the next one goes to the main inventory, not the
        // offhand.
        let next = inv.give(it("minecraft:stone"), 1).unwrap();
        assert!((9..FIRST_HOTBAR).contains(&next), "went to slot {next}");
    }

    #[test]
    fn a_large_gift_is_split_into_stacks() {
        let mut inv = Inventory::new();
        assert_eq!(
            inv.give_many(it("minecraft:stone"), 200),
            0,
            "all of it fitted"
        );
        assert_eq!(inv.count_of(it("minecraft:stone")), 200);
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
        assert_eq!(inv.give_many(it("minecraft:stone"), capacity + 100), 100);
        assert_eq!(inv.count_of(it("minecraft:stone")), capacity);
    }

    #[test]
    fn giving_exactly_a_stack_uses_exactly_one_slot() {
        let mut inv = Inventory::new();
        assert_eq!(inv.give_many(it("minecraft:stone"), 64), 0);
        assert_eq!(inv.occupied().count(), 1);
    }

    #[test]
    fn giving_into_a_full_inventory_fails_rather_than_overwriting() {
        let mut inv = Inventory::new();
        while inv.give(it("minecraft:stone"), 1).is_some() {}
        assert_eq!(inv.give(it("minecraft:diamond"), 1), None);
        assert_eq!(inv.count_of(it("minecraft:diamond")), 0);
    }

    #[test]
    fn taking_spans_slots_and_reports_a_short_fall() {
        let mut inv = Inventory::new();
        inv.set(36, Some((it("minecraft:stone"), 30)));
        inv.set(37, Some((it("minecraft:stone"), 30)));
        assert_eq!(inv.count_of(it("minecraft:stone")), 60);
        assert_eq!(inv.take(it("minecraft:stone"), 45), 45);
        assert_eq!(inv.count_of(it("minecraft:stone")), 15);
        // Asking for more than is there takes what there is and says so,
        // rather than failing and leaving the caller to guess.
        assert_eq!(inv.take(it("minecraft:stone"), 100), 15);
        assert!(inv.is_empty());
    }

    #[test]
    fn taking_leaves_other_items_alone() {
        let mut inv = Inventory::new();
        inv.set(36, Some((it("minecraft:stone"), 10)));
        inv.set(37, Some((it("minecraft:diamond"), 10)));
        inv.take(it("minecraft:stone"), 100);
        assert_eq!(inv.count_of(it("minecraft:diamond")), 10);
    }

    #[test]
    fn an_inventory_survives_a_round_trip_with_its_identities_intact() {
        let mut inv = Inventory::new();
        inv.set(0, Some((it("minecraft:crafting_table"), 1)));
        inv.set(FIRST_HOTBAR, Some((it("minecraft:stone"), 64)));
        inv.set(OFFHAND, Some((it("minecraft:shield"), 1)));
        inv.select(5);
        let back = decode(&encode(&inv)).expect("decodes");
        assert_eq!(back, inv, "uids and slots must both survive");
        assert_eq!(back.held_slot(), 5);
    }

    #[test]
    fn a_potion_bottle_keeps_its_contents_across_a_round_trip() {
        let mut inv = Inventory::new();
        inv.put_slot(
            36,
            Some(Stack::potion(
                "minecraft:potion",
                crate::game::brewing::potion_id("minecraft:long_swiftness").unwrap(),
                1,
            )),
        );
        inv.put_slot(37, Some(Stack::named("minecraft:potion", 1)));
        let back = decode(&encode(&inv)).expect("decodes");
        assert_eq!(back, inv);
        assert_eq!(
            back.get(36).unwrap().potion_id(),
            crate::game::brewing::potion_id("minecraft:long_swiftness")
        );
        assert_eq!(back.get(37).unwrap().potion_id(), None);
    }

    #[test]
    fn a_version_four_inventory_loads_without_potion_contents() {
        // A version 4 blob has no potion field; its stacks read as no potion
        // rather than as a shifted layout.
        let mut inv = Inventory::new();
        inv.put_slot(36, Some(Stack::named("minecraft:stone", 5)));
        let mut b = encode(&inv);
        b[0] = 4;
        // Drop the two-byte potion field of the one occupied slot: version,
        // held, count u16, then slot, count, uid, damage, item.
        b.truncate(b.len() - 2);
        let back = decode(&b).expect("a version 4 blob loads");
        assert_eq!(back.get(36).map(|s| s.count), Some(5));
        assert_eq!(back.get(36).unwrap().potion_id(), None);
    }

    #[test]
    fn an_empty_inventory_round_trips() {
        let inv = Inventory::new();
        assert_eq!(decode(&encode(&inv)), Some(inv));
    }

    #[test]
    fn an_item_uid_is_eight_bytes_on_the_wire() {
        // One stack: version, held, count u16, then slot, count, uid, damage,
        // item id u16. Read by hand so the width is pinned by value.
        let mut inv = Inventory::new();
        inv.set(36, Some((it("minecraft:stone"), 64)));
        inv.slot_mut(36).unwrap().as_mut().unwrap().uid = ItemUid(0x0102_0304_0506_0708);
        let b = encode(&inv);
        assert_eq!(b[0], FORMAT_VERSION);
        assert_eq!(&b[6..14], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_blob_from_another_version_is_refused() {
        let mut b = encode(&Inventory::new());
        b[0] = FORMAT_VERSION.wrapping_add(1);
        assert_eq!(decode(&b), None);
        b[0] = 0;
        assert_eq!(decode(&b), None);
        // Versions 1-2 stored 16-byte uids where this version stores 8; reading
        // one as the other would shift every field after the first uid, so a
        // non-empty one is refused (an empty one has nothing to misread; see
        // `an_old_empty_inventory_loads_as_empty_rather_than_locking_the_player_out`).
        for v in [1u8, 2] {
            let mut full = vec![v, 0, 0, 1];
            full.extend_from_slice(&[0u8; 40]);
            assert_eq!(decode(&full), None, "v{v}");
        }
    }

    #[test]
    fn a_truncated_blob_is_refused_at_every_length() {
        // Never a partial inventory: handing a player half their items is
        // worse than handing them none and saying so.
        let mut inv = Inventory::new();
        inv.set(36, Some((it("minecraft:stone"), 64)));
        inv.set(37, Some((it("minecraft:diamond_pickaxe"), 1)));
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
        assert_eq!(k[0], aether_api::keys::PLAYER_INVENTORY[0]);
        // Journal event keys are 9 bytes and start with 'E'; column indexes
        // are 17 bytes but start with 'X'; actor indexes are 25.
        assert_ne!(k[0], b'E');
        assert_ne!(k[0], b'X');
        assert_ne!(k[0], b'C');
        // The pre-rename key is still readable, and is the old `V`.
        assert_eq!(legacy_key(1)[0], b'V');
        assert_ne!(key(1), legacy_key(1));
    }
}
