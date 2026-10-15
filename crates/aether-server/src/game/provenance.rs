//! Item provenance for the survival layer: every hop an item instance makes
//! goes into the journal.
//!
//! Each stack carries a uid ([`crate::inventory::Stack::uid`]), but until now
//! only `/give` and the economy wrote item events. Picking up, dropping,
//! chests, furnaces and crafting left no trace, so the history could say
//! where a block went but not where a diamond did (Design Notes §10.3).
//!
//! Rather than an event at every line that touches a slot — there are many,
//! across every click mode — an action is wrapped in a [`Scope`]: it captures
//! where each uid the player can reach lies before the action and after it,
//! and journals the difference:
//!
//! * a uid that changed place: [`EventBody::ItemMove`];
//! * a stack at the same place whose count grew because same-item units it
//!   did not mint joined it: [`EventBody::ItemMerge`] from those uids. This
//!   is what distinguishes topping up a stack from a move to the same slot;
//! * a new uid that is units already accounted for, split off a stack:
//!   [`EventBody::ItemMint`], with the source stack moving the other way;
//! * a new uid that is the output of a real transformation (a craft, smelt or
//!   brew): [`EventBody::ItemDerive`] from the ingredient uids that shrank or
//!   vanished in the same action;
//! * a uid that disappeared: [`EventBody::ItemDestroy`], carrying the
//!   [`DestroyReason`] set while the action ran ([`note_destroy_reason`]) —
//!   `consumed` for eating, `crafted` for an ingredient, `other` otherwise.
//!
//! Nothing is invented here but identity: a source that creates units from
//! outside the reachable world (`/give`, a furnaces's output when a stack is
//! merged into) mints them itself (see [`give_events`]) and this layer only
//! records where they went.
//!
//! The difference is journalled when the scope is dropped, so an action that
//! returns early is covered too.
//!
//! The ground is outside what a capture sees, so actions that cross it say so:
//! [`Scope::takes_from_ground`] before a pickup, [`note_thrown`] for a stack thrown
//! out. A stack on the ground is identified by the block it landed at first
//! and keeps that place while it lies there, so a stack that rolls a block
//! downhill is not "moved from somewhere it never was". Items that come into
//! being on the ground — a broken block's drops, mob loot — are minted there
//! directly by [`mint_on_ground`], with the event that produced them as cause.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use aether_world::journal::{ActorId, DestroyReason, EventBody, ItemUid, Place};
use aether_world::ItemTypeId;

use super::player::WindowKind;
use crate::inventory::{GiveStep, Stack};
use crate::players::PlayerHandle;
use crate::session::DemoWorld;

/// The cursor, as a slot of the player's own inventory.
pub const CURSOR_SLOT: i16 = -1;
/// Crafting-table grid cell `i` is slot `GRID_SLOT - i`: the grid belongs to
/// the window, not the table, and goes back to the player when it closes.
pub const GRID_SLOT: i16 = -10;

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    place: Place,
    count: u8,
    item: aether_world::ItemTypeId,
    /// Wear and potion contents are part of an item's identity for
    /// provenance purposes: a water bottle and an awkward bottle share an item
    /// id but are not the same thing, and a brewing step must not be mistaken
    /// for a split of a potion off a potion.
    damage: u16,
    potion: Option<u16>,
}

impl Entry {
    /// Whether two entries are the same *kind* of stack — same item, wear and
    /// potion contents. What a split preserves and a transformation does not.
    fn same_kind(&self, other: &Entry) -> bool {
        self.item == other.item && self.damage == other.damage && self.potion == other.potion
    }
}

type Seen = HashMap<ItemUid, Entry>;

fn add(seen: &mut Seen, s: &Stack, place: Place) {
    if s.count > 0 {
        seen.insert(
            s.uid,
            Entry {
                place,
                count: s.count,
                item: s.item,
                damage: s.damage,
                potion: s.potion,
            },
        );
    }
}

/// Where every item instance this player can reach lies now: inventory,
/// cursor, and the open window's grid or container.
fn capture(handle: &PlayerHandle, world: &DemoWorld) -> Seen {
    let owner = ActorId(handle.uuid);
    let mut seen = Seen::new();
    // Lock order: the game state, then the inventory, then containers.
    let window = {
        let st = handle.game();
        if let Some(c) = &st.cursor {
            add(
                &mut seen,
                c,
                Place::Inventory {
                    owner,
                    slot: CURSOR_SLOT,
                },
            );
        }
        if let Some(super::player::OpenWindow {
            kind: WindowKind::Crafting { grid },
            ..
        }) = &st.window
        {
            for (i, s) in grid.iter().enumerate() {
                if let Some(s) = s {
                    add(
                        &mut seen,
                        s,
                        Place::Inventory {
                            owner,
                            slot: GRID_SLOT - i as i16,
                        },
                    );
                }
            }
        }
        st.window.as_ref().map(|w| w.kind.clone())
    };
    for (i, s) in handle.inventory().occupied() {
        add(
            &mut seen,
            s,
            Place::Inventory {
                owner,
                slot: i as i16,
            },
        );
    }
    let containers: Vec<((i32, i32, i32), super::containers::ContainerKind)> = match &window {
        Some(WindowKind::Chest { left, right }) => {
            let mut v = vec![(*left, super::containers::ContainerKind::Chest)];
            if let Some(r) = right {
                v.push((*r, super::containers::ContainerKind::Chest));
            }
            v
        }
        Some(WindowKind::Furnace { pos }) => {
            vec![(*pos, super::containers::ContainerKind::Furnace)]
        }
        Some(WindowKind::Hopper { pos }) => {
            vec![(*pos, super::containers::ContainerKind::Hopper)]
        }
        Some(WindowKind::Dispenser { pos }) => {
            vec![(*pos, super::containers::ContainerKind::Dispenser)]
        }
        Some(WindowKind::BrewingStand { pos }) => {
            vec![(*pos, super::containers::ContainerKind::BrewingStand)]
        }
        _ => Vec::new(),
    };
    for ((x, y, z), kind) in containers {
        let c = super::containers::snapshot(world, (x, y, z), kind);
        for (slot, s) in c.slots.iter().enumerate() {
            if let Some(s) = s {
                add(
                    &mut seen,
                    s,
                    Place::Container {
                        x,
                        y,
                        z,
                        slot: slot as i16,
                    },
                );
            }
        }
    }
    seen
}

thread_local! {
    /// Stacks thrown to the ground while a scope is open on this thread.
    static THROWN: RefCell<Option<Vec<(Stack, Place)>>> = const { RefCell::new(None) };
    /// Why a destroy inside the running action happened. Set by whatever
    /// consumes the item (eating, a craft) and read once when the scope's
    /// difference is journalled.
    static REASON: Cell<DestroyReason> = const { Cell::new(DestroyReason::Other) };
}

/// Record that an item this action uses up is destroyed for `reason` —
/// `consumed` for eating, `crafted` for a crafting ingredient. The next
/// [`Scope`] to end reads it, so it must be set before the action returns.
pub fn note_destroy_reason(reason: DestroyReason) {
    REASON.with(|r| r.set(reason));
}

fn take_destroy_reason() -> DestroyReason {
    REASON.with(|r| r.replace(DestroyReason::Other))
}

/// The events a [`give`](crate::inventory::Inventory::give_many_report)
/// produced: a mint and, when it topped up an existing stack, the merge that
/// moved the fresh units in. `owner` is the player they were given to.
pub fn give_events(owner: ActorId, item: ItemTypeId, steps: &[GiveStep]) -> Vec<EventBody> {
    let mut out = Vec::new();
    for step in steps {
        match step {
            GiveStep::Placed { slot, stack } => out.push(EventBody::ItemMint {
                uid: stack.uid,
                item: stack.item,
                count: stack.count,
                to: Place::Inventory {
                    owner,
                    slot: *slot as i16,
                },
            }),
            GiveStep::Merged {
                slot,
                into,
                mint,
                count,
            } => {
                let place = Place::Inventory {
                    owner,
                    slot: *slot as i16,
                };
                out.push(EventBody::ItemMint {
                    uid: *mint,
                    item,
                    count: *count,
                    to: place,
                });
                out.push(EventBody::ItemMerge {
                    into: *into,
                    from: *mint,
                    count: *count,
                });
            }
        }
    }
    out
}

/// Hand a single `item` to `inv` and return the events to journal, or `None`
/// when there was no room. A picked-up arrow goes through here: it is a fresh
/// instance, so it mints, and when it tops up a held stack it merges rather
/// than growing that stack's count with no event.
pub fn give_one_events(
    actor: ActorId,
    inv: &mut crate::inventory::Inventory,
    item: ItemTypeId,
) -> Option<Vec<EventBody>> {
    let (steps, left) = inv.give_many_report(item, 1);
    (left == 0).then(|| give_events(actor, item, &steps))
}

/// A stack is leaving a player for the ground at `at` (see [`ground_place`]).
/// Inside a scope it becomes part of that scope's "after"; outside one there
/// is no telling where it came from, so it is minted there.
pub fn note_thrown(world: &DemoWorld, actor: ActorId, stack: &Stack, at: Place) {
    let inside = THROWN.with(|t| match t.borrow_mut().as_mut() {
        Some(v) => {
            v.push((stack.clone(), at));
            true
        }
        None => false,
    });
    if !inside {
        mint_on_ground(world, actor, stack, at, None);
    }
}

/// The place a stack on the ground is known by: the block it first landed at.
pub fn ground_place(x: f64, y: f64, z: f64) -> Place {
    Place::Ground {
        x: x.floor() as i32,
        y: y.floor() as i32,
        z: z.floor() as i32,
    }
}

/// An item that came into being on the ground — a block's drop, mob loot —
/// with the event that produced it, if known.
pub fn mint_on_ground(
    world: &DemoWorld,
    actor: ActorId,
    stack: &Stack,
    at: Place,
    cause: Option<u64>,
) {
    let _ = world.journal().append_caused(
        actor,
        EventBody::ItemMint {
            uid: stack.uid,
            item: stack.item,
            count: stack.count,
            to: at,
        },
        cause,
    );
}

/// `count` units of ground stack `from` joined ground stack `into`. `from` is
/// gone if the merge took all of it; there is no separate destroy for it.
pub fn ground_merged(world: &DemoWorld, into: ItemUid, from: ItemUid, count: u8) {
    let _ = world
        .journal()
        .append(ActorId::SERVER, EventBody::ItemMerge { into, from, count });
}

/// A ground stack left the world on its own — its five minutes ran out, or it
/// fell out of the bottom. `reason` is despawn or void.
pub fn ground_destroyed(world: &DemoWorld, uid: ItemUid, at: Place, reason: DestroyReason) {
    let _ = world.journal().append(
        ActorId::SERVER,
        EventBody::ItemDestroy {
            uid,
            from: at,
            reason,
        },
    );
}

/// A container changed on its own — a furnace burning fuel and smelting —
/// from `before` to `after`: journalled as the difference, by the server.
pub fn container_changed(
    world: &DemoWorld,
    (x, y, z): (i32, i32, i32),
    before: &[Option<Stack>],
    after: &[Option<Stack>],
) {
    if before == after {
        return;
    }
    let seen = |slots: &[Option<Stack>]| {
        let mut seen = Seen::new();
        for (slot, s) in slots.iter().enumerate() {
            if let Some(s) = s {
                add(
                    &mut seen,
                    s,
                    Place::Container {
                        x,
                        y,
                        z,
                        slot: slot as i16,
                    },
                );
            }
        }
        seen
    };
    // An autonomous container change is a furnace or brewing stand working:
    // whatever vanished — the input, the fuel, the ingredient — was used up by
    // a smelt or a brew, so its destroy reason is `Crafted`.
    for body in diff(&seen(before), &seen(after), DestroyReason::Crafted) {
        let _ = world.journal().append(ActorId::SERVER, body);
    }
}

/// One action's worth of item changes for one player; see the module docs.
pub struct Scope<'a> {
    handle: &'a PlayerHandle,
    world: &'a DemoWorld,
    actor: ActorId,
    before: Seen,
    /// Whether this scope owns the thread's thrown-stack list (scopes nest:
    /// only the outermost one journals).
    outermost: bool,
}

impl<'a> Scope<'a> {
    /// Capture `handle`'s items before an action.
    pub fn begin(handle: &'a PlayerHandle, world: &'a DemoWorld) -> Self {
        let outermost = THROWN.with(|t| {
            let mut t = t.borrow_mut();
            if t.is_none() {
                *t = Some(Vec::new());
                true
            } else {
                false
            }
        });
        Scope {
            handle,
            world,
            actor: ActorId(handle.uuid),
            before: if outermost {
                capture(handle, world)
            } else {
                Seen::new()
            },
            outermost,
        }
    }

    /// A stack on the ground the action is about to take from: it is part of
    /// "before", at its ground place.
    pub fn takes_from_ground(&mut self, stack: &Stack, at: Place) {
        add(&mut self.before, stack, at);
    }

    /// What is left of a stack on the ground after the action, if anything:
    /// part of "after".
    pub fn leaves_on_ground(&mut self, stack: &Stack, at: Place) -> &mut Self {
        THROWN.with(|t| {
            if let Some(v) = t.borrow_mut().as_mut() {
                v.push((stack.clone(), at));
            }
        });
        self
    }
}

impl Drop for Scope<'_> {
    /// Capture again and journal the difference — on drop, so an action that
    /// returns early is journalled all the same.
    fn drop(&mut self) {
        if !self.outermost {
            return;
        }
        let thrown = THROWN.with(|t| t.borrow_mut().take()).unwrap_or_default();
        let reason = take_destroy_reason();
        let mut after = capture(self.handle, self.world);
        for (s, at) in &thrown {
            add(&mut after, s, *at);
        }
        for body in diff(&self.before, &after, reason) {
            let _ = self.world.journal().append(self.actor, body);
        }
    }
}

/// The events that turn `before` into `after`: transfers first, then what was
/// made, then what is gone — the order a reader of the ledger expects.
///
/// `reason` is the destroy reason set while the action ran; it applies to
/// every destroy the action leaves behind.
fn diff(before: &Seen, after: &Seen, reason: DestroyReason) -> Vec<EventBody> {
    let mut moves = Vec::new();
    let mut made = Vec::new();
    let mut gone = Vec::new();
    // Units each stack lost this action, by uid. A stack that shrank or
    // vanished is a merge source; it is also what anything new was made of.
    let mut lost: HashMap<ItemUid, u32> = HashMap::new();
    for (uid, b) in before {
        let now = after.get(uid).map_or(0, |a| a.count) as u32;
        if now < b.count as u32 {
            lost.insert(*uid, b.count as u32 - now);
        }
    }
    let mut sources: Vec<ItemUid> = lost.keys().copied().collect();
    sources.sort();

    let mut uids: Vec<ItemUid> = after.keys().copied().collect();
    uids.sort();

    // A stack at the same place whose count grew was topped up. Find the
    // same-item sources that fed it and say so with a merge, rather than a
    // move to the same slot that hid where the units came from.
    let mut merged_from: HashMap<ItemUid, u32> = HashMap::new();
    for uid in &uids {
        let a = &after[uid];
        let Some(b) = before.get(uid) else { continue };
        if b.place != a.place || a.count <= b.count {
            continue;
        }
        let mut need = (a.count - b.count) as u32;
        let mut cands: Vec<ItemUid> = sources
            .iter()
            .copied()
            .filter(|s| before[s].same_kind(a) && lost[s] > *merged_from.get(s).unwrap_or(&0))
            .collect();
        cands.sort();
        for s in cands {
            if need == 0 {
                break;
            }
            let avail = lost[&s] - merged_from.get(&s).copied().unwrap_or(0);
            let take = need.min(avail);
            if take == 0 {
                continue;
            }
            moves.push(EventBody::ItemMerge {
                into: *uid,
                from: s,
                count: take as u8,
            });
            *merged_from.entry(s).or_insert(0) += take;
            need -= take;
        }
        if need > 0 {
            if sources.is_empty() {
                // Nothing fed it: the best a pure difference can say is that
                // its count moved.
                moves.push(EventBody::ItemMove {
                    uid: *uid,
                    from: b.place,
                    to: a.place,
                    count: a.count,
                });
            } else {
                // It grew from a transformation, not from like stacks (a
                // furnace output joining an existing output stack, say): the
                // fresh units are derived from the ingredients and then
                // merged in, so the count did not grow from nowhere.
                let fresh = aether_world::journal::ledger::mint_uid();
                moves.push(EventBody::ItemDerive {
                    uid: fresh,
                    item: a.item,
                    count: need as u8,
                    to: a.place,
                    from: sources.clone(),
                });
                moves.push(EventBody::ItemMerge {
                    into: *uid,
                    from: fresh,
                    count: need as u8,
                });
            }
        }
    }

    // New instances: a split if the item matches a source that lost units
    // (units that already existed and got a new identity), a derive if a real
    // transformation consumed other items, a mint from nothing otherwise.
    for uid in &uids {
        if before.contains_key(uid) {
            continue;
        }
        let a = &after[uid];
        // A split preserves the whole identity — item, wear and potion. A
        // brewing step keeps the item id (`minecraft:potion`) but changes the
        // potion, so it is a transformation and must derive, not mint.
        let split = sources.iter().any(|s| before[s].same_kind(a));
        if split || sources.is_empty() {
            made.push(EventBody::ItemMint {
                uid: *uid,
                item: a.item,
                count: a.count,
                to: a.place,
            });
        } else {
            made.push(EventBody::ItemDerive {
                uid: *uid,
                item: a.item,
                count: a.count,
                to: a.place,
                from: sources.clone(),
            });
        }
    }

    let mut old: Vec<&ItemUid> = before.keys().collect();
    old.sort();
    for uid in old {
        let b = &before[uid];
        let used = merged_from.get(uid).copied().unwrap_or(0);
        match after.get(uid) {
            None => {
                let gone_units = lost.get(uid).copied().unwrap_or(0);
                if used == 0 || used < gone_units {
                    // Not (all) merged away: it was destroyed after all.
                    gone.push(EventBody::ItemDestroy {
                        uid: *uid,
                        from: b.place,
                        reason,
                    });
                }
            }
            Some(a) => {
                let residual = lost.get(uid).copied().unwrap_or(0).saturating_sub(used);
                if a.place != b.place || residual > 0 {
                    moves.push(EventBody::ItemMove {
                        uid: *uid,
                        from: b.place,
                        to: a.place,
                        count: a.count,
                    });
                }
            }
        }
    }

    moves.extend(made);
    moves.extend(gone);
    moves
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> ActorId {
        ActorId(7)
    }
    fn inv(slot: i16) -> Place {
        Place::Inventory {
            owner: owner(),
            slot,
        }
    }
    fn e(place: Place, count: u8, item: &str) -> Entry {
        Entry {
            place,
            count,
            item: crate::inventory::item(item),
            damage: 0,
            potion: None,
        }
    }

    /// An entry for a potion bottle with the given `minecraft:potion` registry
    /// index, for the brew tests.
    fn potion_e(place: Place, count: u8, potion: u16) -> Entry {
        Entry {
            place,
            count,
            item: crate::inventory::item("minecraft:potion"),
            damage: 0,
            potion: Some(potion),
        }
    }

    const OTHER: DestroyReason = DestroyReason::Other;

    #[test]
    fn a_stack_moved_between_slots_is_a_move() {
        let before = Seen::from([(ItemUid(1), e(inv(36), 5, "minecraft:dirt"))]);
        let after = Seen::from([(ItemUid(1), e(inv(9), 5, "minecraft:dirt"))]);
        assert_eq!(
            diff(&before, &after, OTHER),
            vec![EventBody::ItemMove {
                uid: ItemUid(1),
                from: inv(36),
                to: inv(9),
                count: 5
            }]
        );
    }

    #[test]
    fn a_craft_derives_its_output_from_what_it_used_up() {
        // Two logs in the grid: one log consumed, four planks on the cursor.
        let before = Seen::from([(ItemUid(1), e(inv(1), 2, "minecraft:oak_log"))]);
        let after = Seen::from([
            (ItemUid(1), e(inv(1), 1, "minecraft:oak_log")),
            (ItemUid(2), e(inv(CURSOR_SLOT), 4, "minecraft:oak_planks")),
        ]);
        let ev = diff(&before, &after, DestroyReason::Crafted);
        assert_eq!(
            ev[0],
            EventBody::ItemMove {
                uid: ItemUid(1),
                from: inv(1),
                to: inv(1),
                count: 1
            }
        );
        assert_eq!(
            ev[1],
            EventBody::ItemDerive {
                uid: ItemUid(2),
                item: crate::inventory::item("minecraft:oak_planks"),
                count: 4,
                to: inv(CURSOR_SLOT),
                from: vec![ItemUid(1)]
            }
        );
    }

    #[test]
    fn a_split_is_a_mint_with_the_source_moving_not_a_derive() {
        // Moving a diamond through the crafting grid and halving it used to be
        // journalled as a diamond "derived" from another diamond — a
        // transformation that never happened. A split mints a new identity for
        // units that already existed and reduces the source.
        let before = Seen::from([(ItemUid(1), e(inv(1), 8, "minecraft:diamond"))]);
        let after = Seen::from([
            (ItemUid(1), e(inv(1), 4, "minecraft:diamond")),
            (ItemUid(2), e(inv(2), 4, "minecraft:diamond")),
        ]);
        let ev = diff(&before, &after, OTHER);
        assert!(
            ev.iter()
                .all(|b| !matches!(b, EventBody::ItemDerive { .. })),
            "a split must not derive: {ev:?}"
        );
        assert!(ev.contains(&EventBody::ItemMint {
            uid: ItemUid(2),
            item: crate::inventory::item("minecraft:diamond"),
            count: 4,
            to: inv(2),
        }));
        assert!(ev.contains(&EventBody::ItemMove {
            uid: ItemUid(1),
            from: inv(1),
            to: inv(1),
            count: 4,
        }));
    }

    #[test]
    fn moving_an_item_through_the_crafting_grid_is_moves_only() {
        // Placement into and out of a grid cell, and between cells, changes
        // nothing about the item — no count, no kind — so it must journal as
        // moves and nothing else. No derive, no mint, no destroy.
        let grid = |i: i16| Place::Inventory {
            owner: owner(),
            slot: GRID_SLOT - i,
        };
        let before = Seen::from([(ItemUid(1), e(inv(9), 4, "minecraft:diamond"))]);
        // Into a grid cell, then to another cell, then back to the inventory.
        let after = Seen::from([(ItemUid(1), e(grid(3), 4, "minecraft:diamond"))]);
        assert_eq!(
            diff(&before, &after, OTHER),
            vec![EventBody::ItemMove {
                uid: ItemUid(1),
                from: inv(9),
                to: grid(3),
                count: 4,
            }]
        );
        let back = Seen::from([(ItemUid(1), e(inv(36), 4, "minecraft:diamond"))]);
        assert_eq!(
            diff(&after, &back, OTHER),
            vec![EventBody::ItemMove {
                uid: ItemUid(1),
                from: grid(3),
                to: inv(36),
                count: 4,
            }]
        );
    }

    #[test]
    fn a_brew_derives_its_bottle_from_the_bottles_and_ingredient_it_used() {
        // A brewing step transforms water bottles into awkward bottles and
        // consumes the nether wart. The bottle keeps its item id, so a
        // classifier that looked only at the item id would call this a split
        // of a potion off a potion and lose the `from` edge. The potion
        // contents are part of the identity, so this is a derive.
        let water = crate::game::brewing::potion_id("minecraft:water").unwrap();
        let awkward = crate::game::brewing::potion_id("minecraft:awkward").unwrap();
        assert_ne!(water, awkward);
        let bottle = crate::inventory::item("minecraft:potion");

        let before = Seen::from([
            (ItemUid(1), potion_e(inv(0), 1, water)),
            (ItemUid(2), e(inv(3), 1, "minecraft:nether_wart")),
        ]);
        let after = Seen::from([(ItemUid(3), potion_e(inv(0), 1, awkward))]);
        let ev = diff(&before, &after, DestroyReason::Crafted);
        assert!(
            ev.iter().all(|b| !matches!(b, EventBody::ItemMint { .. })),
            "a brew must not mint a bottle from nothing: {ev:?}"
        );
        assert!(
            ev.contains(&EventBody::ItemDerive {
                uid: ItemUid(3),
                item: bottle,
                count: 1,
                to: inv(0),
                from: vec![ItemUid(1), ItemUid(2)],
            }),
            "the bottle must derive from the old bottle and the wart: {ev:?}"
        );
        // The wart is gone: an explicit destroy with the crafted reason.
        assert!(ev.contains(&EventBody::ItemDestroy {
            uid: ItemUid(2),
            from: inv(3),
            reason: DestroyReason::Crafted,
        }));
    }

    #[test]
    fn eating_the_last_one_destroys_it_as_consumed() {
        let before = Seen::from([(ItemUid(3), e(inv(36), 1, "minecraft:bread"))]);
        assert_eq!(
            diff(&before, &Seen::new(), DestroyReason::Consumed),
            vec![EventBody::ItemDestroy {
                uid: ItemUid(3),
                from: inv(36),
                reason: DestroyReason::Consumed,
            }]
        );
    }

    #[test]
    fn a_pickup_onto_a_stack_merges_from_the_ground_stack_with_no_orphan() {
        // A ground stack of three diamonds topping up an existing stack of
        // five: the three units join the slot stack and the ground uid ceases.
        // There is no separate destroy, and no move to the same slot that hid
        // where the units came from.
        let ground = Place::Ground { x: 1, y: 64, z: 1 };
        let before = Seen::from([
            (ItemUid(5), e(ground, 3, "minecraft:diamond")),
            (ItemUid(6), e(inv(36), 5, "minecraft:diamond")),
        ]);
        let after = Seen::from([(ItemUid(6), e(inv(36), 8, "minecraft:diamond"))]);
        assert_eq!(
            diff(&before, &after, OTHER),
            vec![EventBody::ItemMerge {
                into: ItemUid(6),
                from: ItemUid(5),
                count: 3,
            }]
        );
    }

    #[test]
    fn a_pickup_into_an_empty_slot_is_a_move() {
        // The stack keeps its identity as it leaves the ground; only the place
        // changes.
        let ground = Place::Ground { x: 1, y: 64, z: 1 };
        let before = Seen::from([(ItemUid(5), e(ground, 3, "minecraft:diamond"))]);
        let after = Seen::from([(ItemUid(5), e(inv(36), 3, "minecraft:diamond"))]);
        assert_eq!(
            diff(&before, &after, OTHER),
            vec![EventBody::ItemMove {
                uid: ItemUid(5),
                from: ground,
                to: inv(36),
                count: 3,
            }]
        );
    }

    #[test]
    fn a_partial_pickup_merges_what_fits_and_moves_the_rest() {
        let ground = Place::Ground { x: 1, y: 64, z: 1 };
        let before = Seen::from([
            (ItemUid(5), e(ground, 5, "minecraft:diamond")),
            (ItemUid(6), e(inv(36), 60, "minecraft:diamond")),
        ]);
        // Four fit into the slot stack; the fifth moves to an empty slot and
        // keeps the ground uid.
        let after = Seen::from([
            (ItemUid(6), e(inv(36), 64, "minecraft:diamond")),
            (ItemUid(5), e(inv(9), 1, "minecraft:diamond")),
        ]);
        let ev = diff(&before, &after, OTHER);
        assert!(ev.contains(&EventBody::ItemMerge {
            into: ItemUid(6),
            from: ItemUid(5),
            count: 4,
        }));
        assert!(ev.contains(&EventBody::ItemMove {
            uid: ItemUid(5),
            from: ground,
            to: inv(9),
            count: 1,
        }));
        assert!(
            !ev.iter()
                .any(|b| matches!(b, EventBody::ItemDestroy { .. })),
            "no units were destroyed: {ev:?}"
        );
    }

    #[test]
    fn nothing_changed_is_nothing_journalled() {
        let s = Seen::from([(ItemUid(1), e(inv(36), 5, "minecraft:dirt"))]);
        assert!(diff(&s, &s.clone(), OTHER).is_empty());
    }

    // -----------------------------------------------------------------------
    // `/give`, arrow pickup and the ledger balance.
    // -----------------------------------------------------------------------

    use crate::inventory::Inventory;
    use aether_world::journal::{Event, Ledger};

    /// A picked-up arrow is a fresh item instance: picking it up onto an
    /// existing arrow stack must mint the arriving unit and merge it in, not
    /// grow the stack's count with no event (which `/audit` would later report
    /// as an anomaly). This is the exact sequence the arrow-pickup branch in
    /// `entities::tick` runs.
    #[test]
    fn a_picked_up_arrow_mints_and_merges_into_a_held_stack() {
        let arrow = crate::inventory::item("minecraft:arrow");
        let mut bag = Inventory::new();
        bag.put_slot(36, Some(Stack::named("minecraft:arrow", 5)));
        let held = bag.slot(36).unwrap().uid;

        let bodies = give_one_events(owner(), &mut bag, arrow).expect("space for one arrow");
        assert_eq!(bag.slot(36).unwrap().count, 6, "topped up in place");

        // One mint of the new unit, then a merge into the held stack.
        assert_eq!(bodies.len(), 2, "{bodies:?}");
        let mint_uid = match &bodies[0] {
            EventBody::ItemMint {
                uid,
                item,
                count: 1,
                ..
            } => {
                assert_eq!(*item, arrow);
                *uid
            }
            other => panic!("expected a mint, got {other:?}"),
        };
        assert_eq!(
            bodies[1],
            EventBody::ItemMerge {
                into: held,
                from: mint_uid,
                count: 1,
            }
        );

        // Replayed, the events keep the stack's count honest.
        let mut events = vec![ev(
            1,
            EventBody::ItemMint {
                uid: held,
                item: arrow,
                count: 5,
                to: inv(36),
            },
        )];
        for (i, body) in bodies.into_iter().enumerate() {
            events.push(ev(i as u64 + 2, body));
        }
        let ledger = Ledger::replay(&events);
        assert!(ledger.anomalies().is_empty(), "{:?}", ledger.anomalies());
        assert!(ledger.units_conserved());
        assert_eq!(ledger.get(held).unwrap().count, 6);
    }

    #[test]
    fn a_picked_up_arrow_into_an_empty_slot_is_a_plain_mint() {
        let arrow = crate::inventory::item("minecraft:arrow");
        let mut bag = Inventory::new();
        let bodies = give_one_events(owner(), &mut bag, arrow).expect("space for one arrow");
        assert_eq!(bodies.len(), 1, "no merge without a stack to top up");
        assert!(matches!(bodies[0], EventBody::ItemMint { count: 1, .. }));
    }

    #[test]
    fn a_picked_up_arrow_with_no_room_is_not_taken() {
        let arrow = crate::inventory::item("minecraft:arrow");
        let mut bag = Inventory::new();
        // Fill every slot with a different, full stack so nothing can merge.
        for slot in (9..46).chain(0..9) {
            bag.put_slot(slot, Some(Stack::named("minecraft:stone", 64)));
        }
        assert!(give_one_events(owner(), &mut bag, arrow).is_none());
    }

    fn ev(seq: u64, body: EventBody) -> Event {
        Event {
            seq,
            at_ms: seq,
            actor: owner(),
            cause: None,
            body,
        }
    }

    /// Reproduce the live bug: a `/give` of golden apples onto an existing
    /// stack must be a mint of the new units plus a merge into the stack, not
    /// a move `inv:39 → inv:39` whose count grew from nowhere. The ledger must
    /// balance.
    #[test]
    fn giving_onto_an_existing_stack_mints_then_merges_and_balances() {
        let item = crate::inventory::item("minecraft:golden_apple");
        let mut inv = Inventory::new();
        inv.put_slot(39, Some(Stack::named("minecraft:golden_apple", 5)));
        let existing = inv.slot(39).unwrap().uid;

        let (steps, left) = inv.give_many_report(item, 3);
        assert_eq!(left, 0, "it all fitted");
        assert_eq!(inv.slot(39).unwrap().count, 8, "topped up in place");

        let mut events = vec![ev(
            1,
            EventBody::ItemMint {
                uid: existing,
                item,
                count: 5,
                to: Place::Inventory {
                    owner: owner(),
                    slot: 39,
                },
            },
        )];
        for (i, body) in give_events(owner(), item, &steps).into_iter().enumerate() {
            events.push(ev(i as u64 + 2, body));
        }

        assert!(
            events
                .iter()
                .any(|e| matches!(&e.body, EventBody::ItemMerge { into, count: 3, .. } if *into == existing)),
            "no merge into the existing stack: {:?}",
            events.iter().map(|e| &e.body).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.body, EventBody::ItemMint { count: 3, .. })),
            "the three added units were never minted"
        );

        let ledger = Ledger::replay(&events);
        assert!(ledger.anomalies().is_empty(), "{:?}", ledger.anomalies());
        let bal = ledger.balance();
        assert!(bal.is_balanced(), "{bal:?}");
        assert_eq!(
            (bal.created, bal.alive, bal.destroyed, bal.merged_away),
            (2, 1, 0, 1)
        );
        assert_eq!(ledger.get(existing).unwrap().count, 8);
    }

    /// A tiny deterministic PRNG, so the property test fails the same way for
    /// everyone.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    #[derive(Clone, Copy)]
    enum Loc {
        Inv(i16),
        Ground(i32, i32, i32),
    }
    impl Loc {
        fn place(self) -> Place {
            match self {
                Loc::Inv(slot) => inv(slot),
                Loc::Ground(x, y, z) => Place::Ground { x, y, z },
            }
        }
    }

    struct Cell {
        uid: ItemUid,
        item: &'static str,
        count: u8,
        loc: Loc,
    }

    fn seen_of(cells: &[Cell]) -> Seen {
        let mut seen = Seen::new();
        for c in cells {
            if c.count > 0 {
                seen.insert(
                    c.uid,
                    Entry {
                        place: c.loc.place(),
                        count: c.count,
                        item: crate::inventory::item(c.item),
                        damage: 0,
                        potion: None,
                    },
                );
            }
        }
        seen
    }

    /// A random walk over the operations the live server performs — moving in
    /// a window, giving, picking up, merging, eating — where every mutation is
    /// fed through the *same* `diff` the server uses (except `/give`, which
    /// mints its own units) and the resulting events are replayed into the
    /// same [`Ledger`]. The test then checks three things the old version did
    /// not:
    ///
    /// * the ledger's live stacks match the simulated cells exactly (a dropped
    ///   event would leave a uid in one and not the other);
    /// * every instance conserves its *units*, not merely its existence — the
    ///   property that a merge emitter is needed to satisfy, because a top-up
    ///   journalled as a move would grow a count with no `merged_in` to
    ///   explain it (`Ledger::units_conserved`);
    /// * at least one `ItemMerge` was actually emitted, so the test is not
    ///   passing by never exercising the merge path.
    ///
    /// Reverting the merge emitter makes this fail: the destination count
    /// grows but its unit ledger does not, and `units_conserved` catches it.
    #[test]
    fn the_ledger_balances_over_random_window_give_pickup_and_eat_sequences() {
        let items = ["minecraft:dirt", "minecraft:diamond", "minecraft:bread"];
        let mut rng = Rng(0xC0FFEE);
        let mut next_uid = 1u64;
        let mut cells: Vec<Cell> = Vec::new();
        let mut seq = 0u64;
        let mut ledger = Ledger::default();
        let mut merges = 0usize;

        for _ in 0..3_000 {
            let op = rng.below(9);
            let before = seen_of(&cells);
            let mut reason = DestroyReason::Other;
            // `/give` units are minted by the caller and merged in explicitly;
            // every other op mutates `cells` and lets `diff` find the change.
            // Doing both for one op is the double-count the old test had.
            let mut explicit: Option<Vec<EventBody>> = None;
            match op {
                // Move a stack to another place.
                0 | 1 => {
                    if !cells.is_empty() {
                        let i = rng.below(cells.len() as u64) as usize;
                        cells[i].loc = random_loc(&mut rng);
                    }
                }
                // Split a stack: a new identity for units that already exist.
                2 => {
                    let eligible: Vec<usize> =
                        (0..cells.len()).filter(|i| cells[*i].count >= 2).collect();
                    if !eligible.is_empty() {
                        let i = eligible[rng.below(eligible.len() as u64) as usize];
                        let take = 1 + rng.below((cells[i].count - 1) as u64) as u8;
                        cells[i].count -= take;
                        cells.push(Cell {
                            uid: ItemUid(next_uid),
                            item: cells[i].item,
                            count: take,
                            loc: random_loc(&mut rng),
                        });
                        next_uid += 1;
                    }
                }
                // `/give` a fresh stack into some free place: a plain mint.
                3 => {
                    let item = items[rng.below(items.len() as u64) as usize];
                    let count = 1 + rng.below(8) as u8;
                    cells.push(Cell {
                        uid: ItemUid(next_uid),
                        item,
                        count,
                        loc: random_loc(&mut rng),
                    });
                    next_uid += 1;
                }
                // `/give` onto an existing stack: mint the fresh units, then
                // merge them in. Explicit only — no `diff` over the grown
                // state.
                4 => {
                    let stackable: Vec<usize> =
                        (0..cells.len()).filter(|i| cells[*i].count < 64).collect();
                    if !stackable.is_empty() {
                        let i = stackable[rng.below(stackable.len() as u64) as usize];
                        let room = 64 - cells[i].count;
                        let n = 1 + rng.below(room as u64) as u8;
                        let mint = ItemUid(next_uid);
                        next_uid += 1;
                        cells[i].count += n;
                        explicit = Some(vec![
                            EventBody::ItemMint {
                                uid: mint,
                                item: crate::inventory::item(cells[i].item),
                                count: n,
                                to: cells[i].loc.place(),
                            },
                            EventBody::ItemMerge {
                                into: cells[i].uid,
                                from: mint,
                                count: n,
                            },
                        ]);
                    }
                }
                // Pick a ground stack up onto a same-item slot stack: a merge
                // from the ground uid. What does not fit stays on the ground.
                5 | 6 => {
                    let ground: Vec<usize> = (0..cells.len())
                        .filter(|i| matches!(cells[*i].loc, Loc::Ground(..)))
                        .collect();
                    if !ground.is_empty() {
                        let g = ground[rng.below(ground.len() as u64) as usize];
                        let candidate = (0..cells.len()).find(|i| {
                            *i != g
                                && cells[*i].item == cells[g].item
                                && matches!(cells[*i].loc, Loc::Inv(_))
                                && cells[*i].count < 64
                        });
                        if let Some(t) = candidate {
                            let room = 64 - cells[t].count;
                            let n = room.min(cells[g].count);
                            cells[t].count += n;
                            cells[g].count -= n;
                        }
                    }
                }
                // Pick a ground stack into a free slot: the same instance moves.
                7 => {
                    let ground: Vec<usize> = (0..cells.len())
                        .filter(|i| matches!(cells[*i].loc, Loc::Ground(..)))
                        .collect();
                    if !ground.is_empty() {
                        let g = ground[rng.below(ground.len() as u64) as usize];
                        cells[g].loc = Loc::Inv(rng.below(46) as i16);
                    }
                }
                // Eat one: a consumed destroy when the last one goes.
                _ => {
                    if !cells.is_empty() {
                        let i = rng.below(cells.len() as u64) as usize;
                        if cells[i].count > 1 {
                            cells[i].count -= 1;
                        } else {
                            cells.remove(i);
                        }
                        reason = DestroyReason::Consumed;
                    }
                }
            }
            cells.retain(|c| c.count > 0);
            let after = seen_of(&cells);
            let bodies = match explicit {
                Some(bodies) => bodies,
                None => diff(&before, &after, reason),
            };
            for body in bodies {
                if matches!(body, EventBody::ItemMerge { .. }) {
                    merges += 1;
                }
                seq += 1;
                ledger.apply(&ev(seq, body));
            }
        }

        assert!(
            ledger.anomalies().is_empty(),
            "the random walk produced anomalies: {:?}",
            ledger.anomalies()
        );
        assert!(
            merges > 0,
            "no merge was ever emitted, so the merge path was never exercised"
        );

        // The ledger's live state must equal the simulated cells: same uids,
        // same counts, same places. A dropped event would show as a uid in one
        // and not the other.
        for c in &cells {
            let inst = ledger
                .get(c.uid)
                .unwrap_or_else(|| panic!("cell uid {} never reached the ledger", c.uid.0));
            assert!(
                !inst.is_destroyed(),
                "cell uid {} is live but the ledger destroyed it",
                c.uid.0
            );
            assert_eq!(inst.count, c.count, "uid {} count", c.uid.0);
            assert_eq!(inst.at, c.loc.place(), "uid {} place", c.uid.0);
        }
        let live_uids: std::collections::HashSet<ItemUid> = cells.iter().map(|c| c.uid).collect();
        for (uid, inst) in ledger.instances() {
            if !inst.is_destroyed() {
                assert!(
                    live_uids.contains(&uid),
                    "ledger holds live uid {} that no cell has",
                    uid.0
                );
            }
        }

        // Instance balance and, the stronger property, per-uid unit balance.
        let bal = ledger.balance();
        assert!(bal.is_balanced(), "{bal:?}");
        assert!(
            ledger.units_conserved(),
            "some instance's units are unaccounted for: {:?}",
            ledger
                .instances()
                .filter(|(_, i)| !i.conserves_units())
                .map(|(uid, i)| (
                    uid.0,
                    i.count,
                    i.minted,
                    i.merged_in,
                    i.merged_out,
                    i.moved_out,
                    i.destroyed_units
                ))
                .collect::<Vec<_>>()
        );
    }

    fn random_loc(rng: &mut Rng) -> Loc {
        if rng.below(2) == 0 {
            Loc::Inv(rng.below(46) as i16)
        } else {
            Loc::Ground(rng.below(16) as i32, 64, rng.below(16) as i32)
        }
    }
}
