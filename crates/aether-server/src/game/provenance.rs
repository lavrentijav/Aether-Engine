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
//! * a uid that changed place or count: [`EventBody::ItemMove`];
//! * a uid that appeared: [`EventBody::ItemDerive`] from the uids that
//!   disappeared or shrank in the same action (a craft's inputs, the stack a
//!   split came off), or [`EventBody::ItemMint`] when nothing did;
//! * a uid that disappeared: [`EventBody::ItemDestroy`] (eaten, merged into
//!   another stack, used up).
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

use std::cell::RefCell;
use std::collections::HashMap;

use aether_world::journal::{ActorId, EventBody, ItemUid, Place};

use super::player::WindowKind;
use crate::inventory::Stack;
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
    item: String,
}

type Seen = HashMap<ItemUid, Entry>;

fn add(seen: &mut Seen, s: &Stack, place: Place) {
    if s.count > 0 {
        seen.insert(
            s.uid,
            Entry {
                place,
                count: s.count,
                item: s.item.clone(),
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
    let container = match window {
        Some(WindowKind::Chest { pos }) => Some((super::containers::ContainerKind::Chest, pos)),
        Some(WindowKind::Furnace { pos }) => Some((super::containers::ContainerKind::Furnace, pos)),
        _ => None,
    };
    if let Some((kind, (x, y, z))) = container {
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
            item: stack.item.clone(),
            count: stack.count,
            to: at,
        },
        cause,
    );
}

/// A stack on the ground changed: merged into a neighbour, or gone (picked up
/// whole, burnt, expired). `count` is what is left, `0` for gone.
pub fn ground_changed(world: &DemoWorld, uid: ItemUid, at: Place, count: u8) {
    let body = if count == 0 {
        EventBody::ItemDestroy { uid, from: at }
    } else {
        EventBody::ItemMove {
            uid,
            from: at,
            to: at,
            count,
        }
    };
    let _ = world.journal().append(ActorId::SERVER, body);
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
    for body in diff(&seen(before), &seen(after)) {
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
        let mut after = capture(self.handle, self.world);
        for (s, at) in &thrown {
            add(&mut after, s, *at);
        }
        for body in diff(&self.before, &after) {
            let _ = self.world.journal().append(self.actor, body);
        }
    }
}

/// The events that turn `before` into `after`: moves first, then what was
/// made, then what is gone — the order a reader of the ledger expects.
fn diff(before: &Seen, after: &Seen) -> Vec<EventBody> {
    let mut moves = Vec::new();
    let mut made = Vec::new();
    let mut gone = Vec::new();
    // What shrank or vanished in this action is what anything new was made of.
    let mut sources: Vec<ItemUid> = before
        .iter()
        .filter(|(uid, b)| after.get(uid).map_or(true, |a| a.count < b.count))
        .map(|(uid, _)| *uid)
        .collect();
    sources.sort();
    let mut uids: Vec<&ItemUid> = after.keys().collect();
    uids.sort();
    for uid in uids {
        let a = &after[uid];
        match before.get(uid) {
            Some(b) if b.place == a.place && b.count == a.count => {}
            Some(b) => moves.push(EventBody::ItemMove {
                uid: *uid,
                from: b.place,
                to: a.place,
                count: a.count,
            }),
            None if sources.is_empty() => made.push(EventBody::ItemMint {
                uid: *uid,
                item: a.item.clone(),
                count: a.count,
                to: a.place,
            }),
            None => made.push(EventBody::ItemDerive {
                uid: *uid,
                item: a.item.clone(),
                count: a.count,
                to: a.place,
                from: sources.clone(),
            }),
        }
    }
    let mut lost: Vec<(&ItemUid, &Entry)> = before
        .iter()
        .filter(|(u, _)| !after.contains_key(u))
        .collect();
    lost.sort_by_key(|(u, _)| **u);
    for (uid, b) in lost {
        gone.push(EventBody::ItemDestroy {
            uid: *uid,
            from: b.place,
        });
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
            item: item.into(),
        }
    }

    #[test]
    fn a_stack_moved_between_slots_is_a_move() {
        let before = Seen::from([(ItemUid(1), e(inv(36), 5, "minecraft:dirt"))]);
        let after = Seen::from([(ItemUid(1), e(inv(9), 5, "minecraft:dirt"))]);
        assert_eq!(
            diff(&before, &after),
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
        let ev = diff(&before, &after);
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
                item: "minecraft:oak_planks".into(),
                count: 4,
                to: inv(CURSOR_SLOT),
                from: vec![ItemUid(1)]
            }
        );
    }

    #[test]
    fn eating_the_last_one_destroys_it() {
        let before = Seen::from([(ItemUid(3), e(inv(36), 1, "minecraft:bread"))]);
        assert_eq!(
            diff(&before, &Seen::new()),
            vec![EventBody::ItemDestroy {
                uid: ItemUid(3),
                from: inv(36)
            }]
        );
    }

    #[test]
    fn a_pickup_derives_the_slot_stack_from_the_ground_stack() {
        let ground = Place::Ground { x: 1, y: 64, z: 1 };
        let before = Seen::from([(ItemUid(5), e(ground, 3, "minecraft:diamond"))]);
        let after = Seen::from([(ItemUid(6), e(inv(36), 3, "minecraft:diamond"))]);
        assert_eq!(
            diff(&before, &after),
            vec![
                EventBody::ItemDerive {
                    uid: ItemUid(6),
                    item: "minecraft:diamond".into(),
                    count: 3,
                    to: inv(36),
                    from: vec![ItemUid(5)]
                },
                EventBody::ItemDestroy {
                    uid: ItemUid(5),
                    from: ground
                }
            ]
        );
    }

    #[test]
    fn nothing_changed_is_nothing_journalled() {
        let s = Seen::from([(ItemUid(1), e(inv(36), 5, "minecraft:dirt"))]);
        assert!(diff(&s, &s.clone()).is_empty());
    }
}
