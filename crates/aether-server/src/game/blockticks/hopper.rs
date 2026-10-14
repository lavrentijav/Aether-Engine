//! Hoppers: the block that moves items between containers.
//!
//! A hopper acts every eight ticks (vanilla's transfer cooldown). On its turn
//! it first pushes one item from its own slots into the container it faces,
//! then — if it did not push, or pushed and still has room — pulls one item
//! from the container directly above it. It is driven by its own
//! [`Scheduler`](crate::game::scheduler::Scheduler), the same shape the
//! falling blocks and fluids use, and it reschedules itself for eight ticks
//! later whenever it moved something. When it has nothing to do it stops; a
//! block change (a player inserting an item, a neighbour placed) wakes it
//! again through [`notify`].
//!
//! Redstone is not modelled, so the `enabled` property is ignored: a hopper is
//! always on regardless of the signal beside it.

use std::sync::{Mutex, OnceLock};

use crate::game::containers;
use crate::game::scheduler::Scheduler;
use crate::session::DemoWorld;

/// A block position.
type Pos = (i32, i32, i32);

/// Ticks after a transfer before a hopper tries again — vanilla's cooldown.
const TICK_DELAY: u64 = 8;

/// Hoppers processed per tick at most.
const BUDGET: usize = 4096;

/// The cells a change can awaken: itself and the six touching it.
const TOUCH: [Pos; 7] = [
    (0, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (1, 0, 0),
    (-1, 0, 0),
    (0, 0, 1),
    (0, 0, -1),
];

fn queue() -> &'static Mutex<Scheduler<Pos>> {
    static Q: OnceLock<Mutex<Scheduler<Pos>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(Scheduler::new()))
}

fn schedule(pos: Pos, delay: u64) {
    queue()
        .lock()
        .unwrap()
        .schedule(pos, crate::game::now() + delay);
}

fn is_hopper(world: &DemoWorld, pos: Pos) -> bool {
    let state = world.get_block(pos.0, pos.1, pos.2);
    super::name_of(state).is_some_and(|n| n == "minecraft:hopper")
}

/// Wake the hoppers that could move something because of a change at
/// `(x, y, z)`: the cell itself and the six touching it — the one above, which
/// pulls from it, and the one whose facing points at it.
pub fn notify(world: &DemoWorld, x: i32, y: i32, z: i32) {
    for (dx, dy, dz) in TOUCH {
        let p = (x + dx, y + dy, z + dz);
        if is_hopper(world, p) {
            schedule(p, 0);
        }
    }
}

/// Run the hoppers whose cooldown has come up, once per game tick. Returns the
/// containers whose contents changed, so their viewers can be updated.
pub fn tick(world: &DemoWorld) -> Vec<Pos> {
    let now = crate::game::now();
    let batch = queue().lock().unwrap().due(now, BUDGET);
    let mut changed = Vec::new();
    for pos in batch {
        if !is_hopper(world, pos) {
            continue;
        }
        let before = changed.len();
        step(world, pos, &mut changed);
        if changed.len() != before {
            schedule(pos, TICK_DELAY);
        }
    }
    changed
}

/// One hopper's turn: push into what it faces, then pull from what is above.
fn step(world: &DemoWorld, pos: Pos, changed: &mut Vec<Pos>) {
    let state = world.get_block(pos.0, pos.1, pos.2);
    let mut moved = false;
    if let Some(facing) = super::prop(state, "facing") {
        let (dx, dy, dz) = match facing {
            "north" => (0, 0, -1),
            "south" => (0, 0, 1),
            "west" => (-1, 0, 0),
            "east" => (1, 0, 0),
            _ => (0, -1, 0),
        };
        let target = (pos.0 + dx, pos.1 + dy, pos.2 + dz);
        if containers::move_one(world, pos, target) {
            changed.push(pos);
            changed.push(target);
            moved = true;
        }
    }
    let above = (pos.0, pos.1 + 1, pos.2);
    if containers::move_one(world, above, pos) {
        changed.push(above);
        changed.push(pos);
        moved = true;
    }
    if moved {
        // A neighbouring hopper may now have something to move; wake it. The
        // hopper itself is excluded so this does not undo its own cooldown.
        wake_neighbours(world, pos);
    }
}

/// Wake the six hoppers touching `pos`, but not the one at `pos` itself.
fn wake_neighbours(world: &DemoWorld, pos: Pos) {
    for (dx, dy, dz) in TOUCH {
        if (dx, dy, dz) == (0, 0, 0) {
            continue;
        }
        let p = (pos.0 + dx, pos.1 + dy, pos.2 + dz);
        if is_hopper(world, p) {
            schedule(p, 0);
        }
    }
}
