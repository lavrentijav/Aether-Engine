//! Scheduled block behaviour: the blocks that act on their own.
//!
//! A block that acts on its own does not react the instant something changes;
//! it is driven by a tick that arrives a little later, which is exactly what
//! [`super::scheduler`] (the queue [`super::fluids`] uses too) is for.
//!
//! * **Falling blocks** (this file and [`falling`]) — sand, red sand, gravel
//!   and the sixteen concrete powders. Vanilla turns an unsupported one into a
//!   `FallingBlockEntity` two ticks after it, or a neighbour of it, changes,
//!   and the entity falls a block at a time. This engine has no such entity: on
//!   its scheduled tick the block is moved straight to where it would have
//!   landed, which is the same end state without the animation. Concrete powder
//!   also sets when it touches water, before it ever falls.
//!
//! Crops and leaf decay join this module in their own submodules; see
//! `KNOWN_ISSUES` A.11 for what each of them still does not do.

use std::sync::{Mutex, OnceLock};

use aether_api::block_ids;
use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

use super::scheduler::Scheduler;
use crate::game::tables::Rng;
use crate::players::SharedRegistry;
use crate::protocol::ServerEvent;
use crate::session::DemoWorld;

mod crops;
mod falling;
pub mod hopper;
mod leaves;

/// A block position.
type Pos = (i32, i32, i32);

/// Read a block by absolute coordinate. The pure rules take this rather than a
/// world so they can be tested against a plain map, as `placement`'s are.
type Cells<'a> = dyn Fn(i32, i32, i32) -> BlockStateId + 'a;

/// The lowest and highest block a change may be written at.
const WORLD_BOTTOM: i32 = -64;
const WORLD_TOP: i32 = 384;

/// Updates processed per tick at most.
const BUDGET: usize = 4096;

/// Ticks before a falling block notices it is unsupported (vanilla's 2).
const FALL_DELAY: u64 = 2;

/// Ticks before a leaf recomputes its `distance` after a neighbour changes
/// (vanilla's `updateShape` schedules a tick with delay 1).
const LEAF_DELAY: u64 = 1;

/// Ticks before sugar cane or cactus checks it still has support (vanilla's
/// `tick` runs one tick after the change).
const SUPPORT_DELAY: u64 = 1;

/// Blocks sampled per 16³ section per tick — vanilla's default
/// `randomTickSpeed`.
const RANDOM_TICK_SPEED: usize = 3;

const AROUND: [Pos; 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];
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
    queue().lock().unwrap().schedule(pos, super::now() + delay);
}

// ---------------------------------------------------------------------------
// Reading states
// ---------------------------------------------------------------------------

fn name_of(state: BlockStateId) -> Option<&'static str> {
    blocks::block_of_state(state).map(|(_, n)| n)
}

fn short(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn is_named(state: BlockStateId, name: &str) -> bool {
    name_of(state).is_some_and(|n| n == name)
}

fn is_water(state: BlockStateId) -> bool {
    is_named(state, "minecraft:water")
}

fn is_liquid(state: BlockStateId) -> bool {
    super::fluids::cell_of(state).is_some()
}

// ---------------------------------------------------------------------------
// Kinds of block this module cares about
// ---------------------------------------------------------------------------

fn is_falling(name: &str) -> bool {
    matches!(short(name), "sand" | "red_sand" | "gravel")
        || short(name).ends_with("_concrete_powder")
}

fn is_concrete_powder(name: &str) -> bool {
    short(name).ends_with("_concrete_powder")
}

/// Whether a falling block may move into a cell holding `state`
/// (`FallingBlock.isFree`): air, fire, a fluid, or a block marked replaceable.
///
/// The replaceable set is small and does *not* include flowers, saplings,
/// torches or sugar cane — a block has to ask for it — so sand comes to rest on
/// those rather than through them, the way vanilla does.
fn is_free(state: BlockStateId) -> bool {
    if state == block_ids::AIR || is_liquid(state) {
        return true;
    }
    let Some(name) = name_of(state) else {
        return false;
    };
    matches!(
        short(name),
        "cave_air"
            | "void_air"
            | "structure_void"
            | "light"
            | "fire"
            | "soul_fire"
            | "snow"
            | "short_grass"
            | "tall_grass"
            | "fern"
            | "large_fern"
            | "dead_bush"
            | "bush"
            | "short_dry_grass"
            | "tall_dry_grass"
            | "seagrass"
            | "tall_seagrass"
            | "vine"
            | "glow_lichen"
            | "resin_clump"
            | "leaf_litter"
            | "hanging_roots"
            | "bubble_column"
            | "warped_roots"
            | "crimson_roots"
            | "nether_sprouts"
    )
}

// ---------------------------------------------------------------------------
// Reading and writing state properties
// ---------------------------------------------------------------------------

/// The value of `key` in `state`, or `None` if the block has no such property.
fn prop(state: BlockStateId, key: &str) -> Option<&'static str> {
    let (id, _) = blocks::block_of_state(state)?;
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    props::values_of(id, state.raw() - lo)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// `state` with `key` set to `value`, every other property kept, or `None` if
/// the block has no such property.
fn with_prop(state: BlockStateId, key: &str, value: &str) -> Option<BlockStateId> {
    let (id, _) = blocks::block_of_state(state)?;
    let (lo, _) = blocks::STATE_RANGE[id as usize];
    let mut vals = props::values_of(id, state.raw() - lo);
    let slot = vals.iter_mut().find(|(k, _)| *k == key)?;
    slot.1 = value;
    props::state_with(id, &vals).map(BlockStateId)
}

/// Whether `state` is air in any of its flavours.
fn is_air(state: BlockStateId) -> bool {
    state == block_ids::AIR || matches!(name_of(state).map(short), Some("cave_air" | "void_air"))
}

/// Whether this is a leaves block (every one ends `_leaves`).
fn is_leaves(name: &str) -> bool {
    short(name).ends_with("_leaves")
}

/// Whether a random tick should even look at `state`: a crop below its max
/// age, a decaying leaf, or a sugar cane / cactus that may still lengthen.
fn is_random_tickable(state: BlockStateId) -> bool {
    let Some(name) = name_of(state) else {
        return false;
    };
    if is_leaves(name) {
        return leaves::is_decaying(state);
    }
    if crops::is_crop(name) {
        let max = crops::max_age(name).unwrap_or(0);
        let age = prop(state, "age")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(max);
        return age < max;
    }
    crops::is_sugar_cane(name) || crops::is_cactus(name)
}

// ---------------------------------------------------------------------------
// Waking the queue, and running it
// ---------------------------------------------------------------------------

/// A block at `(x, y, z)` changed: wake what it touches.
///
/// The cell itself is included, so a block that has just been placed notices
/// its own rule (sand falls); and the six neighbours, so a block left
/// unsupported — sand over a mined-out cell — notices too.
pub fn notify(world: &DemoWorld, x: i32, y: i32, z: i32) {
    // A hopper beside or above the change may have something to move.
    hopper::notify(world, x, y, z);
    for (dx, dy, dz) in TOUCH {
        let p = (x + dx, y + dy, z + dz);
        let state = world.get_block(p.0, p.1, p.2);
        if state == block_ids::AIR {
            continue;
        }
        let Some(name) = name_of(state) else {
            continue;
        };
        if is_falling(name) {
            schedule(p, FALL_DELAY);
        } else if is_leaves(name) {
            schedule(p, LEAF_DELAY);
        } else if crops::is_sugar_cane(name) || crops::is_cactus(name) {
            schedule(p, SUPPORT_DELAY);
        }
    }
}

/// Run the scheduled ticks, then vanilla's random-tick pass. Called once per
/// game tick.
pub fn tick(world: &DemoWorld, registry: &SharedRegistry, rng: &mut Rng) {
    let now = super::now();
    let batch = queue().lock().unwrap().due(now, BUDGET);
    for pos in batch {
        scheduled_update(world, registry, rng, pos);
    }
    random_ticks(world, registry, rng);
}

fn scheduled_update(world: &DemoWorld, registry: &SharedRegistry, rng: &mut Rng, pos: Pos) {
    let state = world.get_block(pos.0, pos.1, pos.2);
    if state == block_ids::AIR {
        return;
    }
    let Some(name) = name_of(state) else {
        return;
    };
    let get = |x, y, z| world.get_block(x, y, z);
    if is_falling(name) {
        falling::fall(world, registry, pos, state);
    } else if is_leaves(name) {
        leaves::update(world, registry, pos, state);
    } else if (crops::is_sugar_cane(name) && !crops::can_survive_sugar_cane(&get, pos))
        || (crops::is_cactus(name) && !crops::can_survive_cactus(&get, pos))
    {
        destroy(world, registry, rng, pos, state);
    }
}

/// Vanilla's random-tick pass: `RANDOM_TICK_SPEED` random blocks inside every
/// resident 16³ section, of which the ones that act on their own get their
/// tick. The sampling runs under the world's cache read lock, so it only
/// collects positions; the work happens afterwards.
fn random_ticks(world: &DemoWorld, registry: &SharedRegistry, rng: &mut Rng) {
    let mut hits: Vec<Pos> = Vec::new();
    world.for_each_random_tick(
        RANDOM_TICK_SPEED,
        &mut |bound| rng.range(0, bound as i32 - 1) as u32,
        &mut |x, y, z, state| {
            if state != block_ids::AIR && is_random_tickable(state) {
                hits.push((x, y, z));
            }
        },
    );
    for pos in hits {
        random_tick(world, registry, rng, pos);
    }
}

/// One random-tick sample that landed on a block that acts on its own.
fn random_tick(world: &DemoWorld, registry: &SharedRegistry, rng: &mut Rng, pos: Pos) {
    let state = world.get_block(pos.0, pos.1, pos.2);
    let Some(name) = name_of(state) else {
        return;
    };
    let get = |x, y, z| world.get_block(x, y, z);
    if is_leaves(name) {
        leaves::decay(world, registry, rng, pos, state);
    } else if crops::is_crop(name) {
        if let Some(next) = crops::grow_crop(&get, rng, pos, state, name) {
            set(world, registry, pos, next);
        }
    } else if crops::is_sugar_cane(name) {
        if let Some(changes) = crops::grow_sugar_cane(&get, pos, state) {
            for (p, to) in changes {
                set(world, registry, p, to);
            }
        }
    } else if crops::is_cactus(name) {
        if let Some(changes) = crops::grow_cactus(&get, rng, pos, state) {
            for (p, to) in changes {
                set(world, registry, p, to);
            }
        }
    }
}

/// Remove a block that lost its support, dropping its loot — vanilla's
/// `destroyBlock(pos, true)`.
fn destroy(
    world: &DemoWorld,
    registry: &SharedRegistry,
    rng: &mut Rng,
    pos: Pos,
    state: BlockStateId,
) {
    let Some(name) = name_of(state) else {
        return;
    };
    let stacks: Vec<crate::inventory::Stack> = crate::game::tables::block_drops(name, None, rng)
        .into_iter()
        .map(|(item, n)| crate::inventory::Stack::named(&item, n))
        .collect();
    if !stacks.is_empty() {
        crate::game::entities::drop_at_block(
            pos.0,
            pos.1,
            pos.2,
            stacks,
            aether_world::journal::ActorId::SERVER,
            None,
        );
    }
    set(world, registry, pos, block_ids::AIR);
}

/// Write a block, tell everyone, and wake what the change touches.
///
/// No journal: like a flowing fluid, these are consequences of the world, not
/// a player's edit, and recording each one would bury the history `/lookup`
/// and `/rollback` read.
fn set(world: &DemoWorld, registry: &SharedRegistry, pos: Pos, to: BlockStateId) {
    let from = world.get_block(pos.0, pos.1, pos.2);
    if from == to || !(WORLD_BOTTOM..WORLD_TOP).contains(&pos.1) {
        return;
    }
    world.set_block_id(pos.0, pos.1, pos.2, to, world.props_of(to));
    registry.broadcast(
        &ServerEvent::BlockChange {
            x: pos.0,
            y: pos.1,
            z: pos.2,
            block: to,
        },
        world,
    );
    super::fluids::notify(world, pos.0, pos.1, pos.2);
    notify(world, pos.0, pos.1, pos.2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Map(HashMap<Pos, BlockStateId>);

    impl Map {
        fn new() -> Self {
            Self(HashMap::new())
        }
        fn put(mut self, pos: Pos, name: &str) -> Self {
            self.0.insert(pos, blocks::default_state(name).unwrap());
            self
        }
        fn get(&self) -> impl Fn(i32, i32, i32) -> BlockStateId + '_ {
            move |x, y, z| self.0.get(&(x, y, z)).copied().unwrap_or(block_ids::AIR)
        }
    }

    #[test]
    fn the_falling_blocks_are_recognised() {
        for name in [
            "minecraft:sand",
            "minecraft:red_sand",
            "minecraft:gravel",
            "minecraft:white_concrete_powder",
            "minecraft:black_concrete_powder",
        ] {
            assert!(is_falling(name), "{name} should fall");
        }
        for name in [
            "minecraft:stone",
            "minecraft:white_concrete",
            "minecraft:oak_log",
        ] {
            assert!(!is_falling(name), "{name} should not fall");
        }
    }

    #[test]
    fn only_vanillas_replaceable_blocks_let_sand_through() {
        assert!(is_free(block_ids::AIR));
        assert!(is_free(blocks::default_state("minecraft:water").unwrap()));
        assert!(is_free(
            blocks::default_state("minecraft:short_grass").unwrap()
        ));
        assert!(is_free(blocks::default_state("minecraft:fire").unwrap()));
        // A flower is *not* replaceable; sand rests on it.
        assert!(!is_free(blocks::default_state("minecraft:poppy").unwrap()));
        assert!(!is_free(
            blocks::default_state("minecraft:oak_sapling").unwrap()
        ));
        assert!(!is_free(blocks::default_state("minecraft:stone").unwrap()));
    }

    #[test]
    fn sand_over_air_lands_on_the_floor() {
        let m = Map::new().put((0, 0, 0), "minecraft:stone");
        // Sand at y=5 falls to sit on the stone.
        assert_eq!(falling::landing(&m.get(), (0, 5, 0)), Some((0, 1, 0)));
    }

    #[test]
    fn supported_sand_does_not_move() {
        let m = Map::new()
            .put((0, 0, 0), "minecraft:stone")
            .put((0, 1, 0), "minecraft:sand");
        assert_eq!(falling::landing(&m.get(), (0, 1, 0)), None);
    }

    #[test]
    fn sand_falls_through_water() {
        let m = Map::new()
            .put((0, 0, 0), "minecraft:stone")
            .put((0, 1, 0), "minecraft:water")
            .put((0, 2, 0), "minecraft:water");
        // The sand at y=4 comes to rest in the lowest free cell, y=1.
        assert_eq!(falling::landing(&m.get(), (0, 4, 0)), Some((0, 1, 0)));
    }
}
