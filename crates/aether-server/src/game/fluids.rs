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

use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};

use aether_api::block_ids;
use aether_world::registry::{blocks, props};
use aether_world::BlockStateId;

use super::scheduler::Scheduler;

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

// ---------------------------------------------------------------------------
// The flow vector, and pushing bodies with it
// ---------------------------------------------------------------------------

/// Vanilla's `Entity.updateInWaterStateAndDoFluidPushing` motion scale.
pub const FLOW_PUSH: f64 = 0.014;

/// Vanilla fluid height for the flow vector: `getOwnHeight`, whose *level*
/// grows downstream (a source is 0, level 1 is 1/9, …… level 7 is 7/9, a
/// falling column is 8/9). This is not the same as [`Cell::amount`], which is
/// what feeds a cell's level.
fn flow_height(c: Cell) -> f64 {
    if c.is_source() {
        0.0
    } else if c.falling() {
        8.0
    } else {
        c.level as f64
    }
}

/// Vanilla's `FlowingFluid.getFlow`: the unit direction fluid at `pos` pushes
/// a body, `None` when the flow balances out.
///
/// Each horizontal neighbour holding the same fluid contributes its own height
/// in that direction; because the height *rises* downstream, the sum points
/// downstream and the body is carried the way the water runs. A source (height
/// 0) contributes nothing itself, but borrows the height of the fluid below it
/// — vanilla's `f == 0` branch — so an ocean source still feeds the flow when a
/// falling column sits under it, unless the neighbour is a block that blocks
/// motion. A falling column is forced downward as soon as a solid face stands
/// beside it. The result is normalized, as vanilla's is.
fn flow_from(
    fluid_at: &dyn Fn(i32, i32, i32) -> Option<Cell>,
    solid_at: &dyn Fn(i32, i32, i32) -> bool,
    pos: Pos,
    cell: Cell,
) -> Option<(f64, f64, f64)> {
    let (x, y, z) = pos;
    let mut vx = 0.0;
    let mut vz = 0.0;
    for (dx, dz) in SIDES {
        let (nx, ny, nz) = (x + dx, y, z + dz);
        let Some(n) = fluid_at(nx, ny, nz) else {
            continue;
        };
        if n.fluid != cell.fluid {
            continue;
        }
        let mut f = flow_height(n);
        let mut f1 = if f > 0.0 { 1.0 } else { 0.0 };
        if f == 0.0 && !solid_at(nx, ny, nz) {
            if let Some(b) = fluid_at(nx, ny - 1, nz) {
                if b.fluid == cell.fluid {
                    f = flow_height(b);
                    f1 = if f > 0.0 { 1.0 } else { 0.0 };
                }
            }
        }
        vx += dx as f64 * f * f1;
        vz += dz as f64 * f * f1;
    }
    let mut vy = 0.0;
    if cell.falling() {
        for (dx, dz) in SIDES {
            if solid_at(x + dx, y, z + dz) || solid_at(x + dx, y + 1, z + dz) {
                vy = -6.0;
                break;
            }
        }
    }
    let len = (vx * vx + vy * vy + vz * vz).sqrt();
    (len > 1e-9).then(|| (vx / len, vy / len, vz / len))
}

/// The flow vector at the fluid cell containing `(x, y, z)`, or `None`.
pub fn flow_at<B, G>(world: &aether_api::World<B, G>, pos: Pos) -> Option<(f64, f64, f64)>
where
    B: aether_world::KvBackend,
    G: aether_worldgen::ChunkGenerator,
{
    let cell = fluid_cell(world, pos.0, pos.1, pos.2)?;
    let fluid_at = |x, y, z| fluid_cell(world, x, y, z);
    let solid_at = |x, y, z| world.props_of(world.get_block(x, y, z)).collision;
    flow_from(&fluid_at, &solid_at, pos, cell)
}

/// Push a body by the fluid flow it is standing in, vanilla's `0.014` per
/// tick. A body tall enough to span two fluid cells of different flow has both
/// summed, then the total normalized, exactly as vanilla's per-block scan does.
pub fn push_body<B, G>(world: &aether_api::World<B, G>, body: &mut aether_api::Body)
where
    B: aether_world::KvBackend,
    G: aether_worldgen::ChunkGenerator,
{
    let f = body.feet();
    let mut total = (0.0, 0.0, 0.0);
    for h in [0.1, 0.6, 1.1] {
        let p = (
            f.x.floor() as i32,
            (f.y + h).floor() as i32,
            f.z.floor() as i32,
        );
        if let Some(v) = flow_at(world, p) {
            total.0 += v.0;
            total.1 += v.1;
            total.2 += v.2;
        }
    }
    let len = (total.0 * total.0 + total.1 * total.1 + total.2 * total.2).sqrt();
    if len <= 1e-9 {
        return;
    }
    body.velocity.x += total.0 / len * FLOW_PUSH;
    body.velocity.y += total.1 / len * FLOW_PUSH;
    body.velocity.z += total.2 / len * FLOW_PUSH;
}

fn queue() -> &'static Mutex<Scheduler<Pos>> {
    static Q: OnceLock<Mutex<Scheduler<Pos>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(Scheduler::new()))
}

fn schedule(pos: Pos, delay: u64) {
    queue().lock().unwrap().schedule(pos, super::now() + delay);
}

/// Persist the pending fluid ticks, one blob per chunk, beside the chunks.
pub fn save(store: &dyn super::tickstore::TickStore) {
    let entries = queue().lock().unwrap().entries();
    super::tickstore::save(store, super::tickstore::FLUID_TICKS, entries);
}

/// Restore the pending fluid ticks of the chunk `(cx, cz)` when it loads.
pub fn load_column(store: &dyn super::tickstore::TickStore, cx: i32, cz: i32) {
    let entries = super::tickstore::load(store, super::tickstore::FLUID_TICKS, cx, cz);
    if entries.is_empty() {
        return;
    }
    super::tickstore::restore(&mut queue().lock().unwrap(), entries);
}

/// Whether a block state carries `waterlogged=true`.
///
/// Such a block is a contained water source: it never spreads on its own (it
/// is not a fluid block and is never scheduled), but vanilla's fluid logic
/// reads its `FluidState` as a source — it feeds neighbouring flows, counts
/// toward infinite-water creation and gives lava something to harden against.
pub fn is_waterlogged(id: BlockStateId) -> bool {
    let Some((bid, _)) = blocks::block_of_state(id) else {
        return false;
    };
    let (lo, _) = blocks::STATE_RANGE[bid as usize];
    props::values_of(bid, id.raw() - lo)
        .into_iter()
        .any(|(k, v)| k == "waterlogged" && v == "true")
}

/// The fluid cell at `(x, y, z)`: a water/lava block, or a waterlogged block
/// standing in as a water source.
fn fluid_cell<B, G>(world: &aether_api::World<B, G>, x: i32, y: i32, z: i32) -> Option<Cell>
where
    B: aether_world::KvBackend,
    G: aether_worldgen::ChunkGenerator,
{
    let id = world.get_block(x, y, z);
    if let Some(c) = cell_of(id) {
        return Some(c);
    }
    is_waterlogged(id).then_some(Cell {
        fluid: Fluid::Water,
        level: 0,
    })
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
/// weaker flow of the same fluid. Never a source, never a solid block, never a
/// waterlogged block (it is occupied).
fn can_replace<B, G>(world: &aether_api::World<B, G>, id: BlockStateId, fluid: Fluid) -> bool
where
    B: aether_world::KvBackend,
    G: aether_worldgen::ChunkGenerator,
{
    if id == block_ids::AIR {
        return true;
    }
    if let Some(c) = cell_of(id) {
        return c.fluid == fluid && !c.is_source();
    }
    if is_waterlogged(id) {
        return false;
    }
    let props = world.props_of(id);
    if props.collision {
        return false;
    }
    let name = blocks::block_of_state(id)
        .map(|(_, n)| n)
        .unwrap_or_default();
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
fn recompute<B, G>(world: &aether_api::World<B, G>, pos: Pos, fluid: Fluid) -> Option<Cell>
where
    B: aether_world::KvBackend,
    G: aether_worldgen::ChunkGenerator,
{
    let (x, y, z) = pos;
    let above = fluid_cell(world, x, y + 1, z);
    if above.is_some_and(|a| a.fluid == fluid) {
        return Some(Cell { fluid, level: 8 });
    }
    let mut best = 0u8;
    let mut sources = 0;
    for (dx, dz) in SIDES {
        if let Some(n) = fluid_cell(world, x + dx, y, z + dz) {
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
        let firm = world.props_of(below).collision
            || fluid_cell(world, x, y - 1, z).is_some_and(|c| c.is_source());
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
fn spread_dirs<B, G>(world: &aether_api::World<B, G>, pos: Pos, fluid: Fluid) -> Vec<(i32, i32)>
where
    B: aether_world::KvBackend,
    G: aether_worldgen::ChunkGenerator,
{
    let (x, y, z) = pos;
    let mut best = i32::MAX;
    let mut dirs = Vec::new();
    for (dx, dz) in SIDES {
        let first = (x + dx, y, z + dz);
        let id = world.get_block(first.0, y, first.2);
        if !can_replace(world, id, fluid)
            || fluid_cell(world, first.0, y, first.2).is_some_and(|c| c.is_source())
        {
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
fn distance_to_drop<B, G>(world: &aether_api::World<B, G>, start: Pos, fluid: Fluid) -> i32
where
    B: aether_world::KvBackend,
    G: aether_worldgen::ChunkGenerator,
{
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
                fluid_cell(world, x + dx, y + dy, z + dz).is_some_and(|c| c.fluid == Fluid::Water)
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
    let below_cell = fluid_cell(world, below.0, below.1, below.2);
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
                fluid_cell(world, x + dx, y, z + dz)
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
        match fluid_cell(world, p.0, p.1, p.2) {
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
    let batch: Vec<Pos> = queue().lock().unwrap().due(now, BUDGET);
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
                    .map(|(i, n)| crate::inventory::Stack::named(&i, n))
                    .collect(),
                aether_world::journal::ActorId::SERVER,
                None,
            );
        }
    }
    world.set_block_id(x, y, z, to, world.props_of(to));
    registry.broadcast(&ServerEvent::BlockChange { x, y, z, block: to }, world);
    // A fluid change is still a change: sand above it may now be unsupported,
    // a crop beside it may have lost its support.
    super::blockticks::notify(world, x, y, z);
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

    /// A map-of-cells fluid source, for the pure flow tests.
    fn channel(cells: &[(Pos, u8)]) -> impl Fn(i32, i32, i32) -> Option<Cell> {
        let map: std::collections::HashMap<Pos, Cell> = cells
            .iter()
            .map(|&(p, level)| {
                (
                    p,
                    Cell {
                        fluid: Fluid::Water,
                        level,
                    },
                )
            })
            .collect();
        move |x, y, z| map.get(&(x, y, z)).copied()
    }

    #[test]
    fn flow_points_downstream_in_a_channel() {
        let fluid = channel(&[((0, 0, 0), 0), ((1, 0, 0), 1), ((2, 0, 0), 2)]);
        let solid = |_: i32, _: i32, _: i32| false;
        let at = |p| flow_from(&fluid, &solid, p, fluid(p.0, p.1, p.2).unwrap());
        // A source contributes nothing itself, but the level-1 cell to its
        // east does, so the source's cell pushes east too — downstream.
        let (sx, sy, sz) = at((0, 0, 0)).unwrap();
        assert!(
            sx > 0.9 && sy == 0.0 && sz == 0.0,
            "source pushes east: {sx}"
        );
        // Inside the channel the lower cell to the east wins the balance.
        let (x, _, z) = at((1, 0, 0)).unwrap();
        assert!(x > 0.9 && z == 0.0, "wave flows east: {x},{z}");
        // At the leading edge the only same-fluid neighbour is the higher
        // water behind it, so vanilla's sum points back upstream — a known
        // consequence of `getFlow` reading neighbours only, not momentum.
        let (x, _, _) = at((2, 0, 0)).unwrap();
        assert!(x < -0.9, "leading edge points back at the water: {x}");
    }

    #[test]
    fn a_still_pool_has_no_flow() {
        let fluid = channel(&[((0, 0, 0), 0), ((1, 0, 0), 0), ((2, 0, 0), 0)]);
        let solid = |_: i32, _: i32, _: i32| false;
        assert!(flow_from(&fluid, &solid, (1, 0, 0), fluid(1, 0, 0).unwrap()).is_none());
    }

    #[test]
    fn a_falling_column_beside_a_wall_is_pushed_down() {
        // A falling cell (level 8) with a solid block east: vanilla adds the
        // downward −6, so the flow leaves mostly downward.
        let fluid = channel(&[((0, 0, 0), 8)]);
        let solid = |x: i32, _: i32, _: i32| x == 1;
        let (x, y, z) = flow_from(&fluid, &solid, (0, 0, 0), fluid(0, 0, 0).unwrap()).unwrap();
        assert!(y < -0.9, "pushed down against the wall: {y}");
        assert!(x == 0.0 && z == 0.0);
    }

    /// A state of `name` carrying `waterlogged=true`.
    fn waterlogged_state(name: &str) -> BlockStateId {
        for raw in blocks::states_of(name).expect("block exists") {
            let id = BlockStateId(raw);
            if is_waterlogged(id) {
                return id;
            }
        }
        panic!("{name} has no waterlogged state");
    }

    #[test]
    fn only_waterlogged_true_states_count() {
        let wet = waterlogged_state("minecraft:oak_stairs");
        assert!(is_waterlogged(wet));
        assert!(!is_waterlogged(
            blocks::default_state("minecraft:oak_stairs").unwrap()
        ));
        assert!(!is_waterlogged(block_ids::WATER));
        assert!(!is_waterlogged(block_ids::STONE));
    }

    #[test]
    fn a_waterlogged_block_is_a_source_that_feeds_and_combines() {
        use aether_api::{FlatGenerator, MemStore, World};
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        // Firm ground so an infinite source can form over it.
        let stone = blocks::default_state("minecraft:stone").unwrap();
        world.set_block_id(1, 9, 0, stone, world.props_of(stone));
        let stairs = waterlogged_state("minecraft:oak_stairs");
        let sp = world.props_of(stairs);
        world.set_block_id(0, 10, 0, stairs, sp);

        // A waterlogged neighbour is a source: a lone weak cell fed by it
        // strengthens to level 1 instead of drying up.
        let weak = state_of(Cell {
            fluid: Fluid::Water,
            level: 7,
        });
        world.set_block_id(1, 10, 0, weak, world.props_of(weak));
        assert_eq!(
            recompute(&world, (1, 10, 0), Fluid::Water),
            Some(Cell {
                fluid: Fluid::Water,
                level: 1
            })
        );

        // Two sources — the waterlogged block and a real one — make the cell
        // between them a source.
        let src = state_of(Cell {
            fluid: Fluid::Water,
            level: 0,
        });
        world.set_block_id(2, 10, 0, src, world.props_of(src));
        assert_eq!(
            recompute(&world, (1, 10, 0), Fluid::Water),
            Some(Cell {
                fluid: Fluid::Water,
                level: 0
            })
        );
        // The waterlogged block has no waterlogged state of its own to
        // replace: flow cannot occupy it.
        assert!(!can_replace(&world, stairs, Fluid::Water));
    }

    #[test]
    fn push_body_carries_a_body_with_the_flow() {
        use aether_api::{FlatGenerator, MemStore, Vector3, World};
        let world = World::new(MemStore::new(), FlatGenerator::classic());
        for (x, level) in [(0, 0u8), (1, 1), (2, 2)] {
            let id = state_of(Cell {
                fluid: Fluid::Water,
                level,
            });
            world.set_block_id(x, 10, 0, id, world.props_of(id));
        }
        let mut body = aether_api::Body::player(Vector3::new(1.5, 10.0, 0.5));
        push_body(&world, &mut body);
        assert!(body.velocity.x > 0.0, "pushed east: {:?}", body.velocity);
        assert!(body.velocity.y.abs() < 1e-9 && body.velocity.z.abs() < 1e-9);
        // A body out of the water is untouched.
        let mut dry = aether_api::Body::player(Vector3::new(8.5, 40.0, 0.5));
        push_body(&world, &mut dry);
        assert_eq!(dry.velocity.x, 0.0);
    }
}
