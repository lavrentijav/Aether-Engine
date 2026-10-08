//! Flowing water and lava.
//!
//! A port of the shape of vanilla's `FlowingFluid`, driven by a queue of
//! scheduled updates rather than by every block ticking:
//!
//! * a change next to a fluid — a block broken, placed or blown up, a bucket
//!   emptied — schedules that fluid for an update after its delay (water 5
//!   ticks, lava 30);
//! * an update recomputes the cell from its neighbours (a source stays; a
//!   flowing cell is fed from above or from the strongest side, one level
//!   weaker per block — two for lava — and dries up when nothing feeds it;
//!   two water sources either side of a cell over solid ground make a third),
//!   then spreads: straight down first, and sideways only towards the nearest
//!   drop within reach (4 blocks for water, 2 for lava), or everywhere when
//!   there is none;
//! * lava meeting water turns to obsidian (a source) or cobblestone, and lava
//!   falling onto water makes stone.
//!
//! Every change is broadcast as a block update and schedules the neighbours,
//! so a flow settles over a few seconds the way it does in vanilla. The work
//! per tick is capped, so a flood never stalls the tick.
//!
//! Fluid cells are written without an author: they are a consequence of a
//! player's edit, not an edit of their own, and recording each one would bury
//! the history `/lookup` and `/rollback` read under thousands of entries.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};

use aether_api::block_ids;
use aether_world::BlockStateId;

use crate::players::SharedRegistry;
use crate::protocol::ServerEvent;
use crate::session::DemoWorld;

type Pos = (i32, i32, i32);

/// Updates processed per tick at most.
const BUDGET: usize = 2048;

/// The two fluids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fluid {
    Water,
    Lava,
}

impl Fluid {
    fn delay(self) -> u64 {
        match self {
            Fluid::Water => 5,
            Fluid::Lava => 30,
        }
    }
    /// Level lost per block of sideways flow.
    fn drop(self) -> u8 {
        match self {
            Fluid::Water => 1,
            Fluid::Lava => 2,
        }
    }
    /// How far sideways a flow looks for a way down.
    fn slope(self) -> i32 {
        match self {
            Fluid::Water => 4,
            Fluid::Lava => 2,
        }
    }
}

/// A fluid cell: which fluid, and its `level` property — 0 a source, 1..=7
/// flowing (weaker as it rises), 8 and up falling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub fluid: Fluid,
    pub level: u8,
}

impl Cell {
    fn is_source(self) -> bool {
        self.level == 0
    }
    fn falling(self) -> bool {
        self.level >= 8
    }
    /// Vanilla's "amount": 8 for a source or a falling column, `8 - level`
    /// for a flowing cell.
    fn amount(self) -> u8 {
        if self.level == 0 || self.level >= 8 {
            8
        } else {
            8 - self.level
        }
    }
}

struct States {
    water: u32,
    lava: u32,
    obsidian: BlockStateId,
    cobblestone: BlockStateId,
    stone: BlockStateId,
}

fn states() -> &'static States {
    static S: OnceLock<States> = OnceLock::new();
    S.get_or_init(|| {
        use aether_world::registry::blocks;
        let first = |n: &str| *blocks::states_of(n).expect("vanilla fluid").start();
        let st = |n: &str| blocks::default_state(n).expect("vanilla block");
        States {
            water: first("minecraft:water"),
            lava: first("minecraft:lava"),
            obsidian: st("minecraft:obsidian"),
            cobblestone: st("minecraft:cobblestone"),
            stone: st("minecraft:stone"),
        }
    })
}

/// The fluid cell a block state is, if it is one.
pub fn cell_of(id: BlockStateId) -> Option<Cell> {
    let s = states();
    let raw = id.raw();
    if (s.water..s.water + 16).contains(&raw) {
        Some(Cell {
            fluid: Fluid::Water,
            level: (raw - s.water) as u8,
        })
    } else if (s.lava..s.lava + 16).contains(&raw) {
        Some(Cell {
            fluid: Fluid::Lava,
            level: (raw - s.lava) as u8,
        })
    } else {
        None
    }
}

/// The block state of a fluid cell.
pub fn state_of(c: Cell) -> BlockStateId {
    let s = states();
    let base = match c.fluid {
        Fluid::Water => s.water,
        Fluid::Lava => s.lava,
    };
    BlockStateId(base + c.level.min(15) as u32)
}

struct Queue {
    due: BTreeMap<u64, Vec<Pos>>,
    pending: HashSet<Pos>,
}

fn queue() -> &'static Mutex<Queue> {
    static Q: OnceLock<Mutex<Queue>> = OnceLock::new();
    Q.get_or_init(|| {
        Mutex::new(Queue {
            due: BTreeMap::new(),
            pending: HashSet::new(),
        })
    })
}

fn schedule(pos: Pos, delay: u64) {
    let mut q = queue().lock().unwrap();
    if q.pending.insert(pos) {
        q.due.entry(super::now() + delay).or_default().push(pos);
    }
}

const SIDES: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const AROUND: [Pos; 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

/// A block at `pos` changed: wake the fluids it touches.
pub fn notify(world: &DemoWorld, x: i32, y: i32, z: i32) {
    for (dx, dy, dz) in std::iter::once((0, 0, 0)).chain(AROUND) {
        let p = (x + dx, y + dy, z + dz);
        if let Some(c) = cell_of(world.get_block(p.0, p.1, p.2)) {
            schedule(p, c.fluid.delay());
        }
    }
}

/// Whether a fluid may flow into a cell holding `id`: air, a plant or a
/// weaker flow of the same fluid. Never a source, never a solid block.
fn can_replace(world: &DemoWorld, id: BlockStateId, fluid: Fluid) -> bool {
    if id == block_ids::AIR {
        return true;
    }
    if let Some(c) = cell_of(id) {
        return c.fluid == fluid && !c.is_source();
    }
    let props = world.props_of(id);
    if props.collision {
        return false;
    }
    let name = super::block_name(world, id).unwrap_or_default();
    matches!(
        name.trim_start_matches("minecraft:"),
        "short_grass"
            | "tall_grass"
            | "fern"
            | "large_fern"
            | "dead_bush"
            | "fire"
            | "snow"
            | "cave_air"
            | "void_air"
            | "short_dry_grass"
            | "tall_dry_grass"
            | "bush"
    ) || name.ends_with("_flower")
        || name.ends_with("_sapling")
        || name.ends_with("_tulip")
        || matches!(
            name.trim_start_matches("minecraft:"),
            "dandelion"
                | "poppy"
                | "blue_orchid"
                | "allium"
                | "azure_bluet"
                | "oxeye_daisy"
                | "cornflower"
                | "lily_of_the_valley"
                | "torch"
                | "wall_torch"
                | "redstone_wire"
                | "rail"
                | "lever"
                | "red_mushroom"
                | "brown_mushroom"
                | "sugar_cane"
        )
}

/// What one update decided: cells to write.
struct Changes(Vec<(Pos, BlockStateId)>);

/// The state a non-source cell should hold given its neighbours.
fn recompute(world: &DemoWorld, pos: Pos, fluid: Fluid) -> Option<Cell> {
    let (x, y, z) = pos;
    let above = cell_of(world.get_block(x, y + 1, z));
    if above.is_some_and(|a| a.fluid == fluid) {
        return Some(Cell { fluid, level: 8 });
    }
    let mut best = 0u8;
    let mut sources = 0;
    for (dx, dz) in SIDES {
        if let Some(n) = cell_of(world.get_block(x + dx, y, z + dz)) {
            if n.fluid != fluid {
                continue;
            }
            if n.is_source() {
                sources += 1;
            }
            best = best.max(n.amount());
        }
    }
    if fluid == Fluid::Water && sources >= 2 {
        let below = world.get_block(x, y - 1, z);
        let firm = world.props_of(below).collision || cell_of(below).is_some_and(|c| c.is_source());
        if firm {
            return Some(Cell { fluid, level: 0 });
        }
    }
    let amount = best.saturating_sub(fluid.drop());
    (amount > 0).then_some(Cell {
        fluid,
        level: 8 - amount,
    })
}

/// Sideways directions a flow from `pos` takes: towards the nearest place it
/// can fall within the slope distance, or all open sides when there is none.
fn spread_dirs(world: &DemoWorld, pos: Pos, fluid: Fluid) -> Vec<(i32, i32)> {
    let (x, y, z) = pos;
    let mut best = i32::MAX;
    let mut dirs = Vec::new();
    for (dx, dz) in SIDES {
        let first = (x + dx, y, z + dz);
        let id = world.get_block(first.0, y, first.2);
        if !can_replace(world, id, fluid) || cell_of(id).is_some_and(|c| c.is_source()) {
            continue;
        }
        let d = distance_to_drop(world, first, fluid);
        match d.cmp(&best) {
            std::cmp::Ordering::Less => {
                best = d;
                dirs.clear();
                dirs.push((dx, dz));
            }
            std::cmp::Ordering::Equal => dirs.push((dx, dz)),
            std::cmp::Ordering::Greater => {}
        }
    }
    dirs
}

/// Blocks from `start` to the nearest cell with a way down, searching up to
/// the fluid's slope distance; `i32::MAX - 1` when there is none.
fn distance_to_drop(world: &DemoWorld, start: Pos, fluid: Fluid) -> i32 {
    let mut seen = HashSet::new();
    let mut frontier = VecDeque::new();
    frontier.push_back((start, 0));
    seen.insert(start);
    while let Some(((x, y, z), d)) = frontier.pop_front() {
        if can_replace(world, world.get_block(x, y - 1, z), fluid) {
            return d;
        }
        if d >= fluid.slope() - 1 {
            continue;
        }
        for (dx, dz) in SIDES {
            let n = (x + dx, y, z + dz);
            if seen.insert(n) && can_replace(world, world.get_block(n.0, y, n.2), fluid) {
                frontier.push_back((n, d + 1));
            }
        }
    }
    i32::MAX - 1
}

/// One scheduled update.
fn update(world: &DemoWorld, pos: Pos) -> Changes {
    let mut out = Changes(Vec::new());
    let (x, y, z) = pos;
    let here = world.get_block(x, y, z);
    let Some(mut cell) = cell_of(here) else {
        return out;
    };
    let s = states();

    // Lava touching water hardens.
    if cell.fluid == Fluid::Lava {
        let wet = AROUND
            .iter()
            .filter(|(_, dy, _)| *dy != -1)
            .any(|(dx, dy, dz)| {
                cell_of(world.get_block(x + dx, y + dy, z + dz))
                    .is_some_and(|c| c.fluid == Fluid::Water)
            });
        if wet {
            let to = if cell.is_source() {
                s.obsidian
            } else {
                s.cobblestone
            };
            out.0.push((pos, to));
            return out;
        }
    }

    // A flowing cell takes its level from what feeds it, or dries up.
    if !cell.is_source() {
        match recompute(world, pos, cell.fluid) {
            None => {
                out.0.push((pos, block_ids::AIR));
                return out;
            }
            Some(next) if next != cell => {
                out.0.push((pos, state_of(next)));
                cell = next;
            }
            Some(_) => {}
        }
    }

    // Down first.
    let below = (x, y - 1, z);
    let below_id = world.get_block(below.0, below.1, below.2);
    let below_cell = cell_of(below_id);
    if cell.fluid == Fluid::Lava && below_cell.is_some_and(|c| c.fluid == Fluid::Water) {
        out.0.push((below, s.stone));
        return out;
    }
    let falls = can_replace(world, below_id, cell.fluid);
    if falls {
        let falling = Cell {
            fluid: cell.fluid,
            level: 8,
        };
        if below_cell != Some(falling) {
            out.0.push((below, state_of(falling)));
        }
        // A falling cell spreads sideways only when it is fed from three
        // sources around it — a pool draining down a hole keeps its edge.
        let sources = SIDES
            .iter()
            .filter(|(dx, dz)| {
                cell_of(world.get_block(x + dx, y, z + dz))
                    .is_some_and(|c| c.fluid == cell.fluid && c.is_source())
            })
            .count();
        if sources < 3 {
            return out;
        }
    } else if !cell.is_source() && below_cell.is_some_and(|c| c.fluid == cell.fluid) {
        // Resting on its own kind: nothing to spread.
        return out;
    }

    // Then sideways.
    let amount = if cell.falling() { 8 } else { cell.amount() };
    let left = amount.saturating_sub(cell.fluid.drop());
    if left == 0 {
        return out;
    }
    let side = Cell {
        fluid: cell.fluid,
        level: 8 - left,
    };
    for (dx, dz) in spread_dirs(world, pos, cell.fluid) {
        let p = (x + dx, y, z + dz);
        let id = world.get_block(p.0, p.1, p.2);
        match cell_of(id) {
            // Only strengthen a weaker flow; never weaken one.
            Some(c) if c.fluid == cell.fluid && (c.falling() || c.amount() >= left) => {}
            _ => out.0.push((p, state_of(side))),
        }
    }
    out
}

/// Run the fluid updates that are due. Called once per game tick.
pub fn tick(world: &DemoWorld, registry: &SharedRegistry) {
    let now = super::now();
    let batch: Vec<Pos> = {
        let mut q = queue().lock().unwrap();
        let mut batch = Vec::new();
        while batch.len() < BUDGET {
            let Some((&t, _)) = q.due.iter().next() else {
                break;
            };
            if t > now {
                break;
            }
            let mut list = q.due.remove(&t).unwrap_or_default();
            let room = BUDGET - batch.len();
            if list.len() > room {
                let rest = list.split_off(room);
                q.due.insert(t, rest);
            }
            for p in &list {
                q.pending.remove(p);
            }
            batch.extend(list);
        }
        batch
    };
    for pos in batch {
        for (p, to) in update(world, pos).0 {
            apply(world, registry, p, to);
        }
    }
}

/// Write one fluid change, tell everyone, and wake what it touches.
fn apply(world: &DemoWorld, registry: &SharedRegistry, p: Pos, to: BlockStateId) {
    let (x, y, z) = p;
    let from = world.get_block(x, y, z);
    if from == to || !(-64..384).contains(&y) {
        return;
    }
    // A plant washed away drops itself, as vanilla's do.
    if from != block_ids::AIR && cell_of(from).is_none() {
        if let Some(name) = super::block_name(world, from) {
            let drops = super::entities::with_rng(|r| super::tables::block_drops(&name, None, r));
            super::entities::drop_at_block(
                x,
                y,
                z,
                drops
                    .into_iter()
                    .map(|(i, n)| crate::inventory::Stack::new(&i, n))
                    .collect(),
            );
        }
    }
    world.set_block_id(x, y, z, to, world.props_of(to));
    registry.broadcast(&ServerEvent::BlockChange { x, y, z, block: to }, world);
    if let Some(c) = cell_of(to) {
        schedule(p, c.fluid.delay());
    }
    for (dx, dy, dz) in AROUND {
        let n = (x + dx, y + dy, z + dz);
        if let Some(c) = cell_of(world.get_block(n.0, n.1, n.2)) {
            schedule(n, c.fluid.delay());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fluid_states_round_trip_through_their_names() {
        for (fluid, name) in [(Fluid::Water, "water"), (Fluid::Lava, "lava")] {
            for level in 0..16u8 {
                let id = state_of(Cell { fluid, level });
                let full = aether_world::registry::props::state_name(id.raw()).unwrap();
                assert_eq!(full, format!("minecraft:{name}[level={level}]"));
                assert_eq!(cell_of(id), Some(Cell { fluid, level }));
            }
        }
        assert_eq!(cell_of(block_ids::STONE), None);
        assert_eq!(
            state_of(Cell {
                fluid: Fluid::Water,
                level: 0
            }),
            block_ids::WATER
        );
    }

    #[test]
    fn amounts_follow_vanilla() {
        let w = |level| Cell {
            fluid: Fluid::Water,
            level,
        };
        assert_eq!(w(0).amount(), 8);
        assert_eq!(w(1).amount(), 7);
        assert_eq!(w(7).amount(), 1);
        assert_eq!(w(8).amount(), 8);
    }
}
