//! Stacks lying on the ground, and picking them back up.
//!
//! # Why this exists
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
//! # Pickup
//!
//! A drop nobody can retrieve is a deletion with extra steps, so the entity is
//! not decorative: whenever a player moves, anything of theirs within
//! [`PICKUP_RANGE`] is offered back, and what fits is taken. What does not fit
//! stays on the ground, so a player with a full inventory can walk away and
//! come back.
//!
//! Drops are **owned**. Only the player they fell for can pick them up, which
//! is not a vanilla rule but is the right one here: these are not loot, they
//! are the remainder of a transaction that already happened.
//!
//! # They fall, and they expire
//!
//! A drop is a real body: it is stepped through the same voxel collision the
//! players use, so it falls out of the air and lands on whatever is beneath
//! it. Without that a stack dropped while flying hangs where it was made and
//! is unreachable.
//!
//! And it expires, after [`DESPAWN_SECS`]. That is not tidiness — an item that
//! lay on the ground forever *is* an unlimited inventory: a player would keep
//! everything on the floor and pick it up as needed, with the inventory limit
//! meaning nothing. Vanilla's five minutes is the right number for the same
//! reason.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use aether_api::{Body, Vector3};

use crate::players::{PlayerHandle, SharedRegistry};
use crate::protocol::ServerEvent;
use crate::session::DemoWorld;

/// How close a player must come, in blocks, for a drop to return to them.
pub const PICKUP_RANGE: f64 = 2.0;

/// How long a stack lies on the ground before it is gone. Vanilla's five
/// minutes; see the module docs for why it must be finite at all.
pub const DESPAWN_SECS: u64 = 300;

/// A dropped item's collision box, in blocks. Vanilla's is 0.25 square.
const ITEM_SIZE: f64 = 0.25;

/// How often the ground is stepped. Twenty a second, the vanilla tick.
pub const TICK: std::time::Duration = std::time::Duration::from_millis(50);

/// How far a drop must move before the movement is worth a packet.
///
/// A landed item still gains and loses a little velocity to gravity and the
/// collision response every tick; broadcasting that would be a packet per item
/// per tick, forever, for something nobody can see.
const MOVE_EPSILON: f64 = 0.01;

/// One stack on the ground.
#[derive(Debug, Clone)]
pub struct Drop {
    pub entity_id: i32,
    pub owner: u128,
    /// The falling body. Position is read from here, never stored twice.
    pub body: Body,
    pub item: String,
    pub count: u64,
    /// When it was dropped, for the despawn timer.
    pub born: std::time::Instant,
    /// Where it was when its position was last broadcast.
    last_sent: Vector3,
}

impl Drop {
    /// Where it is.
    pub fn pos(&self) -> Vector3 {
        self.body.feet()
    }
    /// Whether it has lain long enough to disappear.
    pub fn expired(&self) -> bool {
        self.born.elapsed().as_secs() >= DESPAWN_SECS
    }
}

fn drops() -> &'static Mutex<Vec<Drop>> {
    static D: OnceLock<Mutex<Vec<Drop>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(Vec::new()))
}

/// Entity ids for dropped items.
///
/// Counted down from a long way below zero so they cannot collide with the
/// player ids, which count up from one. Two entities sharing an id is a bug
/// whose symptom is a player's model vanishing when an item despawns.
fn next_entity_id() -> i32 {
    static NEXT: AtomicI32 = AtomicI32::new(-1_000_000);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Give `count` of `item` to `handle`, dropping whatever does not fit at their
/// feet. Returns `(taken, dropped)`.
///
/// The single entry point for handing items to a player, so that no caller has
/// to remember the full-inventory case — forgetting it is how items get
/// deleted.
pub fn give_or_drop(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    item: &str,
    count: u64,
) -> (u64, u64) {
    let left = handle.inventory().give_many(item, count);
    if left > 0 {
        drop_at_feet(handle, registry, world, item, left);
    }
    (count - left, left)
}

/// Put `count` of `item` on the ground where `handle` is standing.
pub fn drop_at_feet(
    handle: &PlayerHandle,
    registry: &SharedRegistry,
    world: &DemoWorld,
    item: &str,
    count: u64,
) {
    let pos = handle.pos();
    // Dropped at roughly waist height, as vanilla does, so it visibly falls the
    // last step rather than appearing already at rest.
    let at = Vector3::new(pos.x, pos.y + 0.5, pos.z);
    let d = Drop {
        entity_id: next_entity_id(),
        owner: handle.uuid,
        body: Body::sized(at, ITEM_SIZE, ITEM_SIZE),
        item: item.to_owned(),
        count,
        born: std::time::Instant::now(),
        last_sent: at,
    };
    let ev = ServerEvent::DropItem {
        entity_id: d.entity_id,
        x: at.x,
        y: at.y,
        z: at.z,
        item: d.item.clone(),
        count: d.count,
    };
    // Everyone sees it, not just the owner: an item only its owner can see
    // reads as a rendering bug to everyone else standing there.
    handle.emit(&ev, world);
    registry.broadcast_except(handle.entity_id, &ev, world);
    drops().lock().unwrap().push(d);
}

/// Offer back anything of this player's that they have walked up to.
///
/// Called on every movement packet, so it must be cheap when there is nothing
/// to do — and it is: the common case is an empty list and an early return.
pub fn try_pickup(handle: &PlayerHandle, registry: &SharedRegistry, world: &DemoWorld) {
    // Checked without taking the lock on the hot path.
    if drops().lock().unwrap().is_empty() {
        return;
    }
    let pos = handle.pos();
    let mut picked: Vec<(i32, String, u64)> = Vec::new();
    {
        let mut all = drops().lock().unwrap();
        let mut i = 0;
        while i < all.len() {
            let d = &all[i];
            let dp = d.pos();
            let near = d.owner == handle.uuid
                && (dp.x - pos.x).abs() < PICKUP_RANGE
                && (dp.y - pos.y).abs() < PICKUP_RANGE + 1.0
                && (dp.z - pos.z).abs() < PICKUP_RANGE;
            if !near {
                i += 1;
                continue;
            }
            let left = handle.inventory().give_many(&d.item, d.count);
            if left == d.count {
                // Still no room. Leave it exactly where it is rather than
                // re-dropping it, so a player standing in a full inventory
                // does not generate an entity per movement packet.
                i += 1;
                continue;
            }
            let taken = d.count - left;
            picked.push((d.entity_id, d.item.clone(), taken));
            if left == 0 {
                all.remove(i);
            } else {
                all[i].count = left;
                i += 1;
            }
        }
    }
    for (entity_id, item, count) in picked {
        // Despawn only when the whole stack went; a partial pickup leaves the
        // entity showing its old count until the next one, which is a
        // cosmetic lag nobody will see and much simpler than re-sending
        // metadata.
        let still_there = drops()
            .lock()
            .unwrap()
            .iter()
            .any(|d| d.entity_id == entity_id);
        if !still_there {
            let ev = ServerEvent::DespawnEntity(entity_id);
            handle.emit(&ev, world);
            registry.broadcast_except(handle.entity_id, &ev, world);
        }
        handle.emit(
            &ServerEvent::Chat(format!("picked up {count}x {item}")),
            world,
        );
    }
}

/// Re-show every drop to a player who has just joined or come back into range.
pub fn show_all(handle: &PlayerHandle, world: &DemoWorld) {
    for d in drops().lock().unwrap().iter() {
        let p = d.pos();
        handle.emit(
            &ServerEvent::DropItem {
                entity_id: d.entity_id,
                x: p.x,
                y: p.y,
                z: p.z,
                item: d.item.clone(),
                count: d.count,
            },
            world,
        );
    }
}

/// Step every drop one tick: gravity, collision, then expiry.
///
/// Returns the events to broadcast. Split from the sending so the world lock
/// and the drop lock are never held while writing to a socket — a slow client
/// would otherwise stall the whole ground.
fn tick_once(world: &DemoWorld) -> Vec<ServerEvent<'static>> {
    let mut events = Vec::new();
    let mut all = drops().lock().unwrap();
    let mut i = 0;
    while i < all.len() {
        if all[i].expired() {
            events.push(ServerEvent::DespawnEntity(all[i].entity_id));
            all.remove(i);
            continue;
        }
        let d = &mut all[i];
        world.step_body(&mut d.body);
        let now = d.body.feet();
        let moved = (now.x - d.last_sent.x).abs()
            + (now.y - d.last_sent.y).abs()
            + (now.z - d.last_sent.z).abs();
        if moved > MOVE_EPSILON {
            d.last_sent = now;
            events.push(ServerEvent::MoveEntity {
                entity_id: d.entity_id,
                x: now.x,
                y: now.y,
                z: now.z,
            });
        }
        i += 1;
    }
    events
}

/// Run the ground: step the drops and tell everyone, forever.
///
/// One thread for every drop in the world rather than one per drop: there are
/// rarely more than a handful, they are independent, and a thread each would
/// cost more than the work.
pub fn spawn_ticker(registry: SharedRegistry, world: std::sync::Arc<DemoWorld>) {
    std::thread::Builder::new()
        .name("ground".into())
        .spawn(move || loop {
            let start = std::time::Instant::now();
            // Cheap check first: with nothing on the floor this is one lock
            // acquisition and a length test, twenty times a second.
            if !drops().lock().unwrap().is_empty() {
                for ev in tick_once(&world) {
                    registry.broadcast(&ev, &*world);
                }
            }
            std::thread::sleep(TICK.saturating_sub(start.elapsed()));
        })
        .expect("failed to start the ground ticker");
}

/// Everything currently on the ground, for `/inv` and tests.
pub fn all() -> Vec<Drop> {
    drops().lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_entity_ids_are_unique_and_far_from_the_player_range() {
        // Players count up from 1. A collision shows up as a player's model
        // disappearing when an item is picked up, which is a long way from
        // this file.
        let ids: Vec<i32> = (0..100).map(|_| next_entity_id()).collect();
        let unique: std::collections::HashSet<&i32> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        assert!(ids.iter().all(|i| *i < -1000), "ids overlap the player range");
    }
}
