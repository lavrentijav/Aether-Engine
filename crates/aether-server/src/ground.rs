//! Handing items to a player, and what happens when they will not fit.
//!
//! Handing a player items can fail: their inventory is full. Three responses
//! are possible and two of them are wrong.
//!
//! * **Delete the remainder.** A player who bought 500 diamonds and had room
//!   for 300 silently loses 200 they paid for.
//! * **Refuse the whole transfer.** The economy has already moved the money —
//!   in a committed transaction — so refusing here leaves them poorer with
//!   nothing to show.
//! * **Drop it on the floor.** The items still exist, the player can see them,
//!   and they can pick them up once they make room. This is what vanilla does
//!   and it is what this module implements.
//!
//! The stack on the floor is an ordinary item entity (see
//! [`crate::game::entities`]): it falls, it is picked up on contact, and it
//! expires after vanilla's five minutes — an item that lay on the ground
//! forever *is* an unlimited inventory. Unlike loot, these drops are
//! **owned**: only the player they fell for can pick them up, because they
//! are the remainder of a transaction that already happened.

use aether_api::Vector3;
use aether_world::journal::ActorId;

use crate::game::{entities, provenance};
use crate::inventory::Stack;
use crate::players::{PlayerHandle, SharedRegistry};
use crate::session::DemoWorld;
use aether_world::ItemTypeId;

/// Give `count` of `item` to `handle`, dropping whatever does not fit at their
/// feet. Returns `(taken, dropped)`.
///
/// The single entry point for handing items to a player, so that no caller has
/// to remember the full-inventory case — forgetting it is how items get
/// deleted.
pub fn give_or_drop(
    handle: &PlayerHandle,
    _registry: &SharedRegistry,
    world: &DemoWorld,
    item: ItemTypeId,
    count: u64,
) -> (u64, u64) {
    let actor = ActorId(handle.uuid);
    let (steps, left) = handle.inventory().give_many_report(item, count);
    // A gift creates units: mint them, and where they topped up an existing
    // stack say so with a merge rather than growing its count from nowhere.
    {
        let journal = world.journal();
        for body in provenance::give_events(actor, item, &steps) {
            let _ = journal.append(actor, body);
        }
    }
    if left > 0 {
        drop_at_feet(handle, world, item, left);
    }
    handle.sync_inventory(world);
    crate::session::save_inventory(handle, world);
    (count - left, left)
}

/// Put `count` of `item` on the ground where `handle` is standing, for them
/// alone to pick up. Each stack is its own instance and is minted on the
/// ground, so a player picking it back up has a uid to follow.
pub fn drop_at_feet(handle: &PlayerHandle, world: &DemoWorld, item: ItemTypeId, count: u64) {
    let actor = ActorId(handle.uuid);
    let pos = handle.pos();
    let max = crate::game::tables::max_stack_id(item).max(1) as u64;
    let at = Vector3::new(pos.x, pos.y + 0.5, pos.z);
    let mut left = count;
    while left > 0 {
        let n = left.min(max);
        left -= n;
        let stack = Stack::new(item, n as u8);
        provenance::mint_on_ground(
            world,
            actor,
            &stack,
            provenance::ground_place(at.x, at.y, at.z),
            None,
        );
        entities::spawn_item(
            at,
            stack,
            Vector3::new(0.0, 0.1, 0.0),
            20,
            Some(handle.uuid),
        );
    }
}
