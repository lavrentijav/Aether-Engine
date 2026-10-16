//! Anti-xray — the server half of the optional module
//! (`docs/design/visibility-culling.md`).
//!
//! Two masking strategies, selected by `[anti_xray] mode`:
//! - **engine** (cheap, default): an ore-like block that is fully enclosed by
//!   opaque blocks is sent as the surrounding stone/deepslate/netherrack, and
//!   revealed again the moment a neighbour changes so it is on the client
//!   before the player can mine to it.
//! - **culling** (the design doc): the pure section graph and breadth-first
//!   search in [`aether_world::visibility`] decide which `16³` sections a
//!   player can reach by sight; then each visible section's own cached cell
//!   mask decides, locally, which of its `4×4×2` AVX-cells are real and which
//!   are sent as solid stone. The atomic unit is the cell, so a
//!   partially-visible section no longer leaks the ores in its hidden cells,
//!   and there is no global per-cell search (which was ~128× the cost and
//!   stalled the tick).
//!
//! Both are [`BlockSource`] wrappers: the codec reads a hidden block/section
//! and is handed the substitute for it, so the existing per-version encoders
//! turn it into the same forms they already speak, without any codec change.
//! Nothing here edits a codec. Off unless `[anti_xray] enabled = true`.
//!
//! The one deliberate simplification, for both modes: the codecs that compute
//! real light ask the same [`BlockSource`] for blocks and for light, so a
//! hidden cell is lit as the stone it is (block light 0, sky light 0). For the
//! common hidden cell — solid rock — that is the real value anyway.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use aether_world::registry::blocks;
use aether_world::visibility::{
    compute_section_graph, default_is_air, default_is_opaque, reachable_sections, BoxDims,
    SectionGraph, SECTION_SIZE,
};
use aether_world::BlockStateId;

use crate::config::{AntiXrayMode, HiddenFill};
use crate::players::PlayerHandle;
use crate::protocol::{BlockSource, GameMode, ServerEvent};

/// The lowest section `y` this world has: `-64 >> 4`.
pub const SECTION_MIN_Y: i32 = -4;
/// The highest section `y`: `(384 - 1) >> 4`.
pub const SECTION_MAX_Y: i32 = 23;
/// Sections in a column.
pub const SECTION_COUNT: usize = (SECTION_MAX_Y - SECTION_MIN_Y + 1) as usize;

/// Cells along X inside a `16³` section (`16 / 4`).
const CELLS_X: i32 = SECTION_SIZE / BoxDims::CELL.x as i32;
/// Cells along Y inside a section (`16 / 2`).
const CELLS_Y: i32 = SECTION_SIZE / BoxDims::CELL.y as i32;
/// Cells along Z inside a section (`16 / 4`).
const CELLS_Z: i32 = SECTION_SIZE / BoxDims::CELL.z as i32;
/// `4×4×2` cells in one `16³` section (`4 × 8 × 4`).
const CELLS_PER_SECTION: usize = (CELLS_X * CELLS_Y * CELLS_Z) as usize;

/// The lowest `4×4×2` cell `y` the world has (`-64 >> 1`).
const CELL_MIN_Y: i32 = (SECTION_MIN_Y * SECTION_SIZE) >> 1;
/// The highest cell `y` (`(384 - 1) >> 1`).
const CELL_MAX_Y: i32 = (SECTION_MAX_Y * SECTION_SIZE + SECTION_SIZE - 1) >> 1;
/// Cells stacked vertically in a column (`224`).
const CELL_Y_COUNT: usize = (CELL_MAX_Y - CELL_MIN_Y + 1) as usize;
/// `4×4×2` cells in one column (`4 × 4 × 224`).
const CELLS_PER_COLUMN: usize = (CELLS_X * CELLS_Z) as usize * CELL_Y_COUNT;
/// `u64` words backing one column's cell mask.
const CELL_MASK_WORDS: usize = CELLS_PER_COLUMN.div_ceil(64);

/// Solid stone, what a hidden cell is filled with by default.
const STONE: BlockStateId = aether_world::registry::ids::STONE;

/// The `4×4×2` cell holding block `(x, y, z)`.
///
/// Arithmetic shift is floor division for powers of two, so this is correct
/// for negative coordinates too.
#[inline]
pub fn cell_of(x: i32, y: i32, z: i32) -> (i32, i32, i32) {
    (x >> 2, y >> 1, z >> 2)
}

/// Index of cell `(x, y, z)` inside its own column's 3584-bit mask, or `None`
/// when the cell lies outside the world's vertical range.
#[inline]
fn cell_mask_index(x: i32, y: i32, z: i32) -> Option<usize> {
    if !(CELL_MIN_Y..=CELL_MAX_Y).contains(&y) {
        return None;
    }
    let ix = (x & 3) as usize;
    let iz = (z & 3) as usize;
    let iy = (y - CELL_MIN_Y) as usize;
    Some(ix | (iz << 2) | (iy << 4))
}

/// A per-column set of hidden `4×4×2` cells, one bit per cell.
///
/// A column is `16 × 384 × 16` blocks: `4 × 224 × 4 = 3584` cells, exactly 56
/// `u64` words, so the mask has no partial trailing word. This is the compact
/// form the reveal planner diffs; a `HashSet` of the same cells is built only
/// when a wrapper needs it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CellMask {
    words: [u64; CELL_MASK_WORDS],
}

impl Default for CellMask {
    fn default() -> Self {
        Self {
            words: [0; CELL_MASK_WORDS],
        }
    }
}

impl CellMask {
    /// Every cell hidden; the mask of a column none of whose cells is visible.
    fn full() -> Self {
        Self {
            words: [u64::MAX; CELL_MASK_WORDS],
        }
    }

    /// Build a mask from the given hidden cells (cells outside the vertical
    /// range are ignored).
    fn from_cells(cells: impl IntoIterator<Item = (i32, i32, i32)>) -> Self {
        let mut mask = Self::default();
        for (x, y, z) in cells {
            mask.set(x, y, z);
        }
        mask
    }

    /// Whether `(x, y, z)` is hidden in this column's mask.
    pub fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        match cell_mask_index(x, y, z) {
            Some(i) => self.words[i >> 6] & (1u64 << (i & 63)) != 0,
            None => false,
        }
    }

    /// Whether no cell of the column is hidden.
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|w| *w == 0)
    }

    /// The number of hidden cells (a diagnostic for tests).
    pub fn len(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    #[inline]
    fn set(&mut self, x: i32, y: i32, z: i32) {
        if let Some(i) = cell_mask_index(x, y, z) {
            self.words[i >> 6] |= 1u64 << (i & 63);
        }
    }

    #[inline]
    fn clear(&mut self, x: i32, y: i32, z: i32) {
        if let Some(i) = cell_mask_index(x, y, z) {
            self.words[i >> 6] &= !(1u64 << (i & 63));
        }
    }

    /// Mark every cell of section `(cx, cy, cz)` hidden.
    fn hide_section(&mut self, cx: i32, cy: i32, cz: i32) {
        for iy in 0..CELLS_Y {
            for iz in 0..CELLS_Z {
                for ix in 0..CELLS_X {
                    self.set(cx * CELLS_X + ix, cy * CELLS_Y + iy, cz * CELLS_Z + iz);
                }
            }
        }
    }

    /// Clear the cells of section `(cx, cy, cz)` whose bit is set in `visible`
    /// (a section-local mask, index `ix | iz << 2 | iy << 4`), so the column
    /// mask agrees with the section's own `SectionCells::visible`.
    fn reveal_section(&mut self, cx: i32, cy: i32, cz: i32, visible: u128) {
        let mut bits = visible;
        while bits != 0 {
            let i = bits.trailing_zeros();
            bits &= bits - 1;
            let ix = (i & 3) as i32;
            let iz = ((i >> 2) & 3) as i32;
            let iy = ((i >> 4) & 7) as i32;
            self.clear(cx * CELLS_X + ix, cy * CELLS_Y + iy, cz * CELLS_Z + iz);
        }
    }

    /// Whether any cell is hidden here but visible in `desired` — a cell the
    /// client holds as `fill` that has become visible, so the column must be
    /// re-sent.
    fn has_extra(&self, desired: &CellMask) -> bool {
        self.words
            .iter()
            .zip(desired.words.iter())
            .any(|(held, want)| held & !want != 0)
    }
}

/// One player's visibility bookkeeping.
///
/// `Default` is the state before the first search: no centre, nothing known.
#[derive(Default)]
pub struct State {
    /// Set by `/visibility off`: this player sees everything.
    pub bypass: bool,
    /// The `16³` section the last search was rooted at.
    center: Option<(i32, i32, i32)>,
    /// A graph inside this player's view changed since the last search. Set
    /// from a writer thread, cleared by the next search.
    dirty: bool,
    /// Bumped on every search. A reveal plan computed for one search id is not
    /// recomputed for the same id unless it was cut short by the budget.
    search_id: u64,
    /// The sections the player can currently reach by sight (the coarse
    /// section BFS). The per-cell mask is derived from these locally, from
    /// each visible section's own cached cell graph, not from a global cell
    /// search.
    visible_sections: HashSet<(i32, i32, i32)>,
    /// The exact `4×4×2` cell the player's feet are in. It is always revealed,
    /// so a player sealed in a one-block pocket is not encased in stone (the
    /// old global search seeded the player's own cell too).
    own_cell: Option<(i32, i32, i32)>,
    /// For each column sent, the set of hidden `4×4×2` cells the client holds
    /// as `fill` (compact per-column bitmask). What the client currently holds.
    sent: HashMap<(i32, i32), CellMask>,
    /// The search id the reveal plan was last computed for.
    planned_search: u64,
    /// Set when a reveal plan hit the per-tick budget, so the next tick
    /// continues it instead of treating the plan as done.
    reveal_pending: bool,
    /// Engine mode: the cells this client already holds real after a reveal,
    /// so a later neighbour change does not re-send them. Cleared when the
    /// column is unloaded or on reset.
    revealed: HashSet<(i32, i32, i32)>,
}

impl State {
    /// Remember a cell as revealed; `true` if it was not already.
    pub fn mark_revealed(&mut self, pos: (i32, i32, i32)) -> bool {
        self.revealed.insert(pos)
    }

    /// Whether a cell has already been revealed to this client.
    pub fn is_revealed(&self, pos: &(i32, i32, i32)) -> bool {
        self.revealed.contains(pos)
    }

    /// Whether the cached reachable set already matches `section` and no
    /// in-view graph has changed since it was computed.
    pub fn is_fresh(&self, section: (i32, i32, i32)) -> bool {
        self.center == Some(section) && !self.dirty
    }

    /// Force the next search to recompute, even at the same centre. Used when
    /// a `/spectate xray` margin change makes the old reachable set wrong.
    pub fn invalidate(&mut self) {
        self.center = None;
    }

    /// Remember the exact cell the player's feet are in, so it is never
    /// masked (a sealed pocket must not encase the player).
    pub fn set_own_cell(&mut self, cell: (i32, i32, i32)) {
        self.own_cell = Some(cell);
    }

    /// Adopt a freshly computed reachable section set.
    ///
    /// `epoch_at_start` is the global edit epoch when the search began. The
    /// stale flag is only cleared if no block was written while the search
    /// ran: a write that landed mid-search is not lost, the next search runs
    /// again.
    pub fn set_visible_if(
        &mut self,
        section: (i32, i32, i32),
        visible_sections: HashSet<(i32, i32, i32)>,
        epoch_at_start: u64,
    ) {
        self.center = Some(section);
        self.visible_sections = visible_sections;
        self.search_id = self.search_id.wrapping_add(1);
        if EDIT_EPOCH.load(Ordering::Relaxed) == epoch_at_start {
            self.dirty = false;
        }
    }

    /// Adopt a reachable section set together with the columns already sent,
    /// for the join path where the first columns go out before a handle
    /// exists.
    pub fn seed(
        &mut self,
        section: (i32, i32, i32),
        visible_sections: HashSet<(i32, i32, i32)>,
        sent: HashMap<(i32, i32), CellMask>,
    ) {
        self.center = Some(section);
        self.dirty = false;
        self.visible_sections = visible_sections;
        self.sent = sent.into_iter().filter(|(_, m)| !m.is_empty()).collect();
        self.search_id = self.search_id.wrapping_add(1);
        self.planned_search = self.search_id;
        self.reveal_pending = false;
    }

    /// Adopt the columns of an engine-mode join burst, so `/visibility off`
    /// knows to re-send them. Engine mode masks individual enclosed ores, so
    /// every loaded column is a possible re-send and the mask content is
    /// irrelevant — only "non-empty" is used.
    pub fn seed_engine(&mut self, section: (i32, i32, i32), columns: &[(i32, i32)]) {
        self.center = Some(section);
        self.dirty = false;
        self.visible_sections.clear();
        self.sent = columns.iter().map(|&c| (c, CellMask::full())).collect();
        self.search_id = self.search_id.wrapping_add(1);
        self.planned_search = self.search_id;
        self.reveal_pending = false;
    }

    /// The hidden-cell mask for column `(cx, cz)` of the current reachable set.
    pub fn hidden_for(&self, world: &dyn BlockSource, cx: i32, cz: i32) -> CellMask {
        let mut mask = column_hidden(world, &self.visible_sections, cx, cz);
        if let Some(c) = self.own_cell {
            if c.0 >> 4 == cx && c.2 >> 4 == cz {
                mask.clear(c.0, c.1, c.2);
            }
        }
        mask
    }

    /// Record the cells a column was actually sent with (an empty mask records
    /// that the client holds the column real).
    pub fn record_sent(&mut self, cx: i32, cz: i32, hidden: CellMask) {
        if hidden.is_empty() {
            self.sent.remove(&(cx, cz));
        } else {
            self.sent.insert((cx, cz), hidden);
        }
    }

    /// Record an engine-mode column as possibly masked (`masked`) or real.
    pub fn record_sent_engine(&mut self, cx: i32, cz: i32, masked: bool) {
        if masked {
            self.sent.insert((cx, cz), CellMask::full());
        } else {
            self.sent.remove(&(cx, cz));
        }
    }

    /// The hidden cells to render column `(cx, cz)` with, updating what the
    /// client holds.
    ///
    /// `bypass` is the caller's current flag. It matters even on the last
    /// column: `/visibility off` re-renders the columns it had masked, and
    /// recording the empty mask here is what tells the planner the client now
    /// holds the real blocks (round-2 N4 / round-3 New-2).
    pub fn mask_for_render(
        &mut self,
        world: &dyn BlockSource,
        cx: i32,
        cz: i32,
        bypass: bool,
    ) -> CellMask {
        let hidden = if bypass {
            CellMask::default()
        } else {
            self.hidden_for(world, cx, cz)
        };
        self.record_sent(cx, cz, hidden.clone());
        hidden
    }

    /// Forget a column the client no longer has.
    pub fn forget(&mut self, cx: i32, cz: i32) {
        self.sent.remove(&(cx, cz));
        // A re-streamed column is masked again, so its revealed cells are no
        // longer what the client holds.
        self.revealed
            .retain(|(x, _, z)| x >> 4 != cx || z >> 4 != cz);
    }

    /// Drop everything: a respawn starts the client with an empty level, so
    /// the columns it used to hold are no longer ours to reveal or mask.
    pub fn reset(&mut self) {
        self.center = None;
        self.dirty = true;
        self.visible_sections.clear();
        self.own_cell = None;
        self.sent.clear();
        self.revealed.clear();
        self.search_id = self.search_id.wrapping_add(1);
        self.planned_search = self.search_id;
        self.reveal_pending = false;
    }

    /// Mark this player's reachable set stale if any of `sections` falls
    /// inside the view box rooted at their current cell.
    ///
    /// A change outside that box cannot alter the answer: the breadth-first
    /// search never looks outside it. The vertical axis is always in range,
    /// because the search walks the whole column.
    pub fn mark_dirty_for(&mut self, view_radius: i32, sections: &[(i32, i32, i32)]) {
        let Some(c) = self.center else {
            return;
        };
        if sections
            .iter()
            .any(|s| (s.0 - c.0).abs() <= view_radius && (s.2 - c.2).abs() <= view_radius)
        {
            self.dirty = true;
        }
    }

    /// Force the next search to run, regardless of what changed.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// The columns to re-send this tick: newly visible ones, nearest first,
    /// at most `budget`. A plan cut short by the budget is continued on the
    /// next tick rather than dropped.
    pub fn reveal_step(&mut self, world: &dyn BlockSource, budget: u32) -> Vec<(i32, i32)> {
        if budget == 0 {
            return Vec::new();
        }
        if self.planned_search == self.search_id && !self.reveal_pending {
            return Vec::new();
        }
        self.planned_search = self.search_id;
        let plan = plan_reveals(
            world,
            &self.visible_sections,
            &self.sent,
            self.center.unwrap_or((0, 0, 0)),
            budget,
        );
        self.reveal_pending = plan.len() as u32 == budget;
        plan
    }

    /// The sections this player can currently see (tests and diagnostics).
    pub fn visible(&self) -> &HashSet<(i32, i32, i32)> {
        &self.visible_sections
    }

    /// The columns sent and their last-known hidden-cell masks (tests).
    pub fn sent(&self) -> &HashMap<(i32, i32), CellMask> {
        &self.sent
    }
}

/// The section-level face graph and the section-local visible-cell mask of one
/// `16³` section, computed together from one snapshot of its blocks.
struct SectionCells {
    /// The section-level face graph the coarse BFS runs on.
    section: SectionGraph,
    /// The section-local cells visible through the section's own openings,
    /// computed once here (world-only, so it is cached and shared): index
    /// `ix | iz << 2 | iy << 4`. This is what gives cell granularity without a
    /// global cell search.
    visible: u128,
}

/// Index of cell `(ix, iy, iz)` (each local to the section) in
/// [`SectionCells::graphs`]: `ix | iz << 2 | iy << 4`.
#[inline]
fn cell_local_index(ix: i32, iy: i32, iz: i32) -> usize {
    (ix & 3) as usize | ((iz & 3) as usize) << 2 | ((iy & 7) as usize) << 4
}

/// The cells of a section visible through its own open faces, as a 128-bit
/// section-local mask (index `ix | iz << 2 | iy << 4`).
///
/// Seeded from every non-opaque boundary cell (a cell on a section face whose
/// face can be seen), flooded through passable (air) cells; every reached
/// non-opaque cell, and every opaque cell touching one, is visible. A cell
/// buried inside solid rock, and an interior pocket no opening reaches, stay
/// hidden. Bounded to the 128 cells of one section — this is the local
/// computation that replaces the global per-cell breadth-first search.
fn compute_cell_visible(non_opaque: u128, air: u128) -> u128 {
    let mut visible = 0u128;
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..CELLS_PER_SECTION {
        if non_opaque >> i & 1 == 0 {
            continue;
        }
        let ix = (i & 3) as i32;
        let iz = ((i >> 2) & 3) as i32;
        let iy = ((i >> 4) & 7) as i32;
        let boundary = ix == 0
            || ix == CELLS_X - 1
            || iz == 0
            || iz == CELLS_Z - 1
            || iy == 0
            || iy == CELLS_Y - 1;
        if !boundary {
            continue;
        }
        visible |= 1u128 << i;
        if air >> i & 1 != 0 {
            stack.push(i);
        }
    }
    while let Some(i) = stack.pop() {
        let ix = (i & 3) as i32;
        let iz = ((i >> 2) & 3) as i32;
        let iy = ((i >> 4) & 7) as i32;
        for (dx, dy, dz) in NEIGHBOURS {
            let (nx, ny, nz) = (ix + dx, iy + dy, iz + dz);
            if !(0..CELLS_X).contains(&nx)
                || !(0..CELLS_Y).contains(&ny)
                || !(0..CELLS_Z).contains(&nz)
            {
                continue;
            }
            let n = cell_local_index(nx, ny, nz);
            if visible >> n & 1 != 0 || non_opaque >> n & 1 == 0 {
                continue;
            }
            visible |= 1u128 << n;
            if air >> n & 1 != 0 {
                stack.push(n);
            }
        }
    }
    // An opaque cell touching a visible cell shows its face, so it is visible
    // too; a second layer of rock is not.
    let mut lit = visible;
    let mut adj = 0u128;
    while lit != 0 {
        let i = lit.trailing_zeros() as usize;
        lit &= lit - 1;
        let ix = (i & 3) as i32;
        let iz = ((i >> 2) & 3) as i32;
        let iy = ((i >> 4) & 7) as i32;
        for (dx, dy, dz) in NEIGHBOURS {
            let (nx, ny, nz) = (ix + dx, iy + dy, iz + dz);
            if !(0..CELLS_X).contains(&nx)
                || !(0..CELLS_Y).contains(&ny)
                || !(0..CELLS_Z).contains(&nz)
            {
                continue;
            }
            adj |= 1u128 << cell_local_index(nx, ny, nz);
        }
    }
    visible | adj
}

/// Compute a section's face graph and its local visible-cell mask from its
/// `4096` blocks (indexed `x | z << 4 | y << 8`).
fn compute_section_cells(blocks: &[BlockStateId; 4096]) -> SectionCells {
    let section = compute_section_graph(blocks, default_is_opaque, default_is_air);
    let mut non_opaque = 0u128;
    let mut air = 0u128;
    for iy in 0..CELLS_Y {
        for iz in 0..CELLS_Z {
            for ix in 0..CELLS_X {
                let mut any_non_opaque = false;
                let mut any_air = false;
                for by in 0..BoxDims::CELL.y as i32 {
                    for bz in 0..BoxDims::CELL.z as i32 {
                        for bx in 0..BoxDims::CELL.x as i32 {
                            let sx = ix * BoxDims::CELL.x as i32 + bx;
                            let sy = iy * BoxDims::CELL.y as i32 + by;
                            let sz = iz * BoxDims::CELL.z as i32 + bz;
                            let block = blocks[(sx | (sz << 4) | (sy << 8)) as usize];
                            any_non_opaque |= !default_is_opaque(block);
                            any_air |= default_is_air(block);
                        }
                    }
                }
                let i = cell_local_index(ix, iy, iz);
                if any_non_opaque {
                    non_opaque |= 1u128 << i;
                }
                if any_air {
                    air |= 1u128 << i;
                }
            }
        }
    }
    let visible = compute_cell_visible(non_opaque, air);
    SectionCells { section, visible }
}

/// The shared section-cell-graph cache.
///
/// The cell graphs of a section depend only on the world, so one cache serves
/// every player. They are computed from a snapshot of the blocks *outside* the
/// lock and stored only if the global edit epoch is unchanged between the
/// snapshot and the store, so a write that lands in that window discards the
/// stale entry (finding 4).
///
/// The epoch is global and monotonic rather than a per-section counter that is
/// reset on store: a per-section reset lets an in-flight compute that read the
/// reset value be re-admitted after a newer graph is stored, poisoning the
/// cache with a pre-write graph (round-3 New-1). A global version that only
/// ever increases cannot be re-created, and it removes the per-section map
/// entirely (round-2 N3).
#[derive(Default)]
struct GraphCache {
    sections: HashMap<(i32, i32, i32), Arc<SectionCells>>,
}

/// Store `cells` for `key` only if no write landed since `computed_at` was
/// read.
fn store_section_cells(
    cache: &mut GraphCache,
    key: (i32, i32, i32),
    cells: Arc<SectionCells>,
    computed_at: u64,
) {
    if EDIT_EPOCH.load(Ordering::Relaxed) == computed_at {
        cache.sections.insert(key, cells);
    }
}

static CACHE: OnceLock<Mutex<GraphCache>> = OnceLock::new();

fn cache() -> &'static Mutex<GraphCache> {
    CACHE.get_or_init(|| Mutex::new(GraphCache::default()))
}

/// The global edit epoch: every block write that could change a graph bumps
/// it, and it is never reset.
///
/// It guards two things. A graph is only cached if it is unchanged since the
/// snapshot was taken (finding 4). A player's search reads it before and after
/// the breadth-first pass, so a write that lands mid-search is not cleared
/// (round-2 N2). It is a single relaxed increment — no lock, no allocation.
static EDIT_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Drop `key`'s cached cell graphs and advance the epoch.
fn invalidate_section(cache: &mut GraphCache, key: (i32, i32, i32)) {
    cache.sections.remove(&key);
    EDIT_EPOCH.fetch_add(1, Ordering::Relaxed);
}

/// Sections written since the last tick, for the tick thread to hand to the
/// players in range. Bounded: drained once a tick.
static DIRTY_SECTIONS: OnceLock<Mutex<HashSet<(i32, i32, i32)>>> = OnceLock::new();

fn dirty_sections() -> &'static Mutex<HashSet<(i32, i32, i32)>> {
    DIRTY_SECTIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Blocks written since the last tick, for `engine` mode. A changed block can
/// expose the ore next to it, so the tick thread re-sends those neighbours to
/// the players in range. Bounded: drained once a tick.
static DIRTY_BLOCKS: OnceLock<Mutex<HashSet<(i32, i32, i32)>>> = OnceLock::new();

fn dirty_blocks() -> &'static Mutex<HashSet<(i32, i32, i32)>> {
    DIRTY_BLOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Forget a section's cached graph after a block in it changed.
///
/// Called from every path that writes a block, so a broken wall reopens the
/// sight line to whatever was behind it. It is deliberately the cheapest thing
/// that can work: with anti-xray off it returns before touching any lock or
/// allocating, so a deployment on the default configuration pays nothing
/// (N1). When on, it takes one lock and records what changed; the tick thread
/// decides which players that actually affects. There is no snapshot of the
/// player list here.
#[inline]
pub fn invalidate_block(x: i32, y: i32, z: i32) {
    if !crate::anti_xray_enabled() {
        return;
    }
    // Always evict the section's cached cell graphs and record the section for
    // the coarse culling pass, regardless of the global mode: a player may hold
    // a per-player `/spectate xray <margin>` override (effective mode culling)
    // while the server runs `engine`, so their cell set must be invalidated by
    // a block write too. The write path is not hot when anti-xray is on.
    record_invalidation((x >> 4, y >> 4, z >> 4));
    if crate::anti_xray_config().mode == AntiXrayMode::Engine {
        dirty_blocks().lock().unwrap().insert((x, y, z));
    }
}

/// The cache/epoch/dirty work for one changed section.
fn record_invalidation(key: (i32, i32, i32)) {
    invalidate_section(&mut cache().lock().unwrap(), key);
    dirty_sections().lock().unwrap().insert(key);
}

/// Drop every cached graph. Used by tests to start from a clean cache.
#[cfg(test)]
pub fn invalidate_all() {
    cache().lock().unwrap().sections.clear();
    dirty_sections().lock().unwrap().clear();
    EDIT_EPOCH.fetch_add(1, Ordering::Relaxed);
}

/// The `4×4×2` cell graphs and section graph of section `s`, computed and
/// cached on first ask.
///
/// `None` outside the world's vertical range, which stops the search at the
/// floor and ceiling.
fn section_cells(world: &dyn BlockSource, s: (i32, i32, i32)) -> Option<Arc<SectionCells>> {
    if !(SECTION_MIN_Y..=SECTION_MAX_Y).contains(&s.1) {
        return None;
    }
    // Read the epoch under the lock, at the same moment the miss is observed,
    // so the version seen is exactly the one that was current when we decided
    // to compute: an invalidation that lands after this point bumps the epoch
    // and is caught at the store (finding 4).
    let computed_at = {
        let cc = cache().lock().unwrap();
        if let Some(cells) = cc.sections.get(&s) {
            return Some(cells.clone());
        }
        EDIT_EPOCH.load(Ordering::Relaxed)
    };
    let mut blocks = [BlockStateId::AIR; 4096];
    world.copy_section(s.0, s.1, s.2, &mut blocks);
    let cells = Arc::new(compute_section_cells(&blocks));
    store_section_cells(&mut cache().lock().unwrap(), s, cells.clone(), computed_at);
    Some(cells)
}

/// The `4×4×2` cell a player's feet are in.
pub fn cell_of_pos(pos: crate::players::PosLook) -> (i32, i32, i32) {
    cell_of(
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    )
}

/// The `16³` section a player's feet are in. The reachable set is keyed on
/// this: the coarse section search is re-run when the player crosses a section
/// border, not on every `4×4×2` cell, so a sub-section move reuses the
/// previous answer. Cell granularity is applied afterwards, per visible
/// section, by [`column_hidden`].
pub fn section_of(pos: crate::players::PosLook) -> (i32, i32, i32) {
    (
        (pos.x.floor() as i32) >> 4,
        (pos.y.floor() as i32) >> 4,
        (pos.z.floor() as i32) >> 4,
    )
}

/// The reachable section set for a bare position, for the join path where the
/// first columns go out before the player's handle exists.
pub fn visible_from(
    world: &dyn BlockSource,
    pos: (f64, f64, f64),
    view_distance: i32,
) -> HashSet<(i32, i32, i32)> {
    visible_from_margin(
        world,
        pos,
        view_distance,
        crate::anti_xray_config().margin_blocks,
    )
}

/// The reachable **section** set with an explicit margin, for the per-player
/// anti-xray override (`/spectate xray <level>`).
///
/// This is the cheap, coarse search: sections, not `4×4×2` cells, so
/// `view_distance` is a view distance in chunks/sections as it has always
/// been. Cell granularity is applied later, per visible section, by
/// [`column_hidden`] from each section's own cached cell graph — never by a
/// global cell search, which would be ~128× the cost.
pub fn visible_from_margin(
    world: &dyn BlockSource,
    pos: (f64, f64, f64),
    view_distance: i32,
    margin_blocks: i32,
) -> HashSet<(i32, i32, i32)> {
    let margin = margin_blocks as f64;
    // One shared lookup per search: the BFS asks for the same section many
    // times, so memoise the (small, `Copy`) section graph locally and touch
    // the process-wide cache — a lock and an atomic refcount — only on the
    // first ask. This is what keeps the all-air worst case in the low
    // milliseconds instead of tens.
    let mut memo: HashMap<(i32, i32, i32), Option<SectionGraph>> = HashMap::new();
    reachable_sections(
        pos,
        margin,
        view_distance,
        SECTION_MIN_Y,
        SECTION_MAX_Y,
        |x, y, z| {
            *memo
                .entry((x, y, z))
                .or_insert_with(|| section_cells(world, (x, y, z)).map(|cells| cells.section))
        },
    )
}

/// Recompute a player's reachable set if the player moved to a new section or
/// an in-view graph changed since the last search.
fn ensure_visible(handle: &PlayerHandle, world: &dyn BlockSource) {
    let pos = handle.pos();
    let cell = cell_of_pos(pos);
    let section = section_of(pos);
    // Track the player's exact cell every call, before the early return: a
    // move within the same section keeps the section set fresh, but the own
    // cell changes, and it must never be masked (or the client would show the
    // player encased in stone for a frame).
    handle.visibility().set_own_cell(cell);
    if handle.visibility().is_fresh(section) {
        return;
    }
    let epoch = EDIT_EPOCH.load(Ordering::Relaxed);
    let visible = visible_from_margin(
        world,
        (pos.x, pos.y, pos.z),
        crate::server_view_radius(),
        effective_margin(handle),
    );
    handle.visibility().set_visible_if(section, visible, epoch);
}

/// The anti-xray mode in force for this player: their `/spectate xray`
/// override, else the server config.
pub fn effective_mode(handle: &PlayerHandle) -> AntiXrayMode {
    match handle.staff().xray() {
        crate::staff::AntiXrayLevel::Engine => AntiXrayMode::Engine,
        crate::staff::AntiXrayLevel::Culling | crate::staff::AntiXrayLevel::Margin(_) => {
            AntiXrayMode::Culling
        }
        _ => crate::anti_xray_config().mode,
    }
}

/// The culling margin in force for this player.
pub fn effective_margin(handle: &PlayerHandle) -> i32 {
    match handle.staff().xray() {
        crate::staff::AntiXrayLevel::Margin(n) => n,
        _ => crate::anti_xray_config().margin_blocks,
    }
}

/// Whether anti-xray is in force for this player: on globally, or turned on for
/// them by a `/spectate xray` override.
pub fn effective_enabled(handle: &PlayerHandle) -> bool {
    handle.staff().xray() != crate::staff::AntiXrayLevel::Default || crate::anti_xray_enabled()
}

/// A human-readable summary of the anti-xray state **for this player**, for
/// `/visibility status`: whether they bypass it (and why) or what mode they
/// are under.
pub fn player_status(handle: &PlayerHandle) -> String {
    if !effective_enabled(handle) {
        return "anti-xray is off (server config)".to_string();
    }
    if crate::anti_xray_bypass(handle) {
        let reason = match handle.staff().xray() {
            crate::staff::AntiXrayLevel::Off => "/spectate xray off",
            _ if handle.visibility().bypass => "/visibility off",
            _ => "an explicit bypass grant",
        };
        return format!("you bypass anti-xray ({reason})");
    }
    let hidden = crate::anti_xray_config().hidden_block_names().len();
    match effective_mode(handle) {
        AntiXrayMode::Engine => format!("anti-xray: engine, {hidden} hidden block(s)"),
        AntiXrayMode::Culling => format!(
            "anti-xray: culling (cells 4x4x2), margin {} blocks, {hidden} hidden block(s)",
            effective_margin(handle)
        ),
    }
}

/// The hidden `4×4×2` cells of column `(cx, cz)` for a reachable **section**
/// set.
///
/// A section not in `visible_sections` is hidden whole (every cell `fill`). A
/// visible section contributes its own cached per-cell visible mask
/// ([`SectionCells::visible`]), so a partly-visible section — the surface, a
/// wall the player stands next to — is masked cell by cell. This is the whole
/// cell-granular path, and it is bounded: it iterates the column's sections,
/// never a global cell search.
pub fn column_hidden(
    world: &dyn BlockSource,
    visible_sections: &HashSet<(i32, i32, i32)>,
    cx: i32,
    cz: i32,
) -> CellMask {
    let mut mask = CellMask::full();
    for i in 0..SECTION_COUNT {
        let cy = SECTION_MIN_Y + i as i32;
        if !visible_sections.contains(&(cx, cy, cz)) {
            continue;
        }
        if let Some(cells) = section_cells(world, (cx, cy, cz)) {
            mask.reveal_section(cx, cy, cz, cells.visible);
        }
    }
    mask
}

/// The fill block for this player.
///
/// `air` is only for a client that cannot fall through the hole it would open;
/// a survival player is given stone even when the config asks for air.
pub fn fill_for(handle: &PlayerHandle) -> BlockStateId {
    fill_for_mode(handle.game().mode)
}

/// The fill block for a game mode, for the join path before a handle exists.
pub fn fill_for_mode(mode: GameMode) -> BlockStateId {
    match crate::anti_xray_config().hidden_fill {
        HiddenFill::Stone => STONE,
        HiddenFill::Air if mode == GameMode::Survival => STONE,
        HiddenFill::Air => BlockStateId::AIR,
    }
}

/// How a column should be masked before the codec encodes it.
pub enum ColumnMask {
    /// Send the column as it is.
    None,
    /// `culling`: encode these hidden `4×4×2` cells as `fill`.
    Cells {
        hidden: Box<CellMask>,
        fill: BlockStateId,
    },
    /// `engine`: wrap the column in an [`EngineColumn`] so enclosed hidden
    /// blocks become their surrounding block.
    Engine,
}

/// Decide how to mask column `(cx, cz)` for this player, recording what the
/// client will hold for the `/visibility off` re-send.
pub fn mask_for_render(
    handle: &PlayerHandle,
    world: &dyn BlockSource,
    cx: i32,
    cz: i32,
) -> ColumnMask {
    if !effective_enabled(handle) {
        return ColumnMask::None;
    }
    let bypass = crate::anti_xray_bypass(handle);
    match effective_mode(handle) {
        AntiXrayMode::Culling => {
            if !bypass {
                ensure_visible(handle, world);
            }
            let hidden = handle.visibility().mask_for_render(world, cx, cz, bypass);
            if hidden.is_empty() {
                ColumnMask::None
            } else {
                ColumnMask::Cells {
                    hidden: Box::new(hidden),
                    fill: fill_for(handle),
                }
            }
        }
        // An engine column is always built (it only pays for the hidden-block
        // lookup); a bypassed player gets the plain view and records nothing.
        AntiXrayMode::Engine => {
            if bypass {
                handle.visibility().record_sent_engine(cx, cz, false);
                ColumnMask::None
            } else {
                ColumnMask::Engine
            }
        }
    }
}

/// Record the mask an engine-mode column was sent with, so `/visibility off`
/// knows to re-send it.
pub fn record_engine(handle: &PlayerHandle, cx: i32, cz: i32, masked: bool) {
    handle.visibility().record_sent_engine(cx, cz, masked);
}

/// Mask a single block change for a player, so a server-side write of a hidden
/// block (`/setblock`, creative, `/rollback`) does not leak it.
///
/// `engine`: an enclosed hidden block becomes its surrounding block.
/// `culling`: a change inside a section this player cannot see becomes the
/// fill the client already holds (usually stone), so nothing appears until the
/// section is revealed whole.
pub fn mask_block(
    handle: &PlayerHandle,
    world: &dyn BlockSource,
    x: i32,
    y: i32,
    z: i32,
    block: BlockStateId,
) -> BlockStateId {
    if !effective_enabled(handle) {
        return block;
    }
    if crate::anti_xray_bypass(handle) {
        return block;
    }
    match effective_mode(handle) {
        AntiXrayMode::Engine => {
            let hidden = hidden_states();
            if hidden.contains(block) && is_buried(world, x, y, z) {
                surrounding_block(world, hidden, x, y, z)
            } else {
                block
            }
        }
        AntiXrayMode::Culling => {
            if cell_hidden(handle, world, x, y, z) {
                fill_for(handle)
            } else {
                block
            }
        }
    }
}

/// Whether the `4×4×2` cell holding `(x, y, z)` is hidden from this player in
/// culling mode.
fn cell_hidden(handle: &PlayerHandle, world: &dyn BlockSource, x: i32, y: i32, z: i32) -> bool {
    if cell_of(x, y, z) == cell_of_pos(handle.pos()) {
        return false;
    }
    ensure_visible(handle, world);
    let section = (x >> 4, y >> 4, z >> 4);
    if !handle.visibility().visible().contains(&section) {
        return true;
    }
    // The section is reachable; its own cached mask says which cells of it the
    // player can actually see.
    match section_cells(world, section) {
        Some(cells) => {
            let local = cell_local_index((x >> 2) & 3, (y >> 1) & 7, (z >> 2) & 3);
            cells.visible & (1u128 << local) == 0
        }
        None => true,
    }
}

/// Forget a column the client no longer has.
pub fn forget_column(handle: &PlayerHandle, cx: i32, cz: i32) {
    handle.visibility().forget(cx, cz);
}

/// The columns whose cell mask is currently non-empty, for `/visibility off`.
fn masked_columns(sent: &HashMap<(i32, i32), CellMask>) -> Vec<(i32, i32)> {
    sent.iter()
        .filter(|(_, mask)| !mask.is_empty())
        .map(|(&column, _)| column)
        .collect()
}

/// Re-send every column this client currently holds masked, so `/visibility
/// off` shows the real blocks without waiting for columns to re-stream.
///
/// The caller sets `bypass` first: with it on, re-encoding a column records an
/// empty mask, so the client ends up with every cell real and the reveal
/// planner has nothing left to do.
pub fn reveal_all(handle: &PlayerHandle, world: &dyn BlockSource) {
    let columns = masked_columns(handle.visibility().sent());
    for (cx, cz) in columns {
        handle.emit(&ServerEvent::ChunkColumn { cx, cz }, world);
    }
}

/// Re-send every column this client currently holds, so a `/spectate xray`
/// level change takes effect at once.
///
/// Unlike [`reveal_all`], this re-sends even columns that were sent real: when
/// the level is *lowered* (turned on, or the margin raised) those columns must
/// come back masked. The reachable set is invalidated first so a margin change
/// is recomputed rather than reused.
pub fn resend_held_columns(handle: &PlayerHandle, world: &dyn BlockSource) {
    handle.visibility().invalidate();
    let columns: Vec<(i32, i32)> = handle.visibility().sent().keys().copied().collect();
    for (cx, cz) in columns {
        handle.emit(&ServerEvent::ChunkColumn { cx, cz }, world);
    }
}

/// Drop the columns a respawned client no longer holds, so the reveal planner
/// stops walking them (finding 7).
pub fn reset_columns(handle: &PlayerHandle) {
    handle.visibility().reset();
}

/// Columns to re-send (reveal) this pass, nearest first, capped at `budget`.
///
/// A reveal is a column that holds a cell the client currently has as `fill`
/// but that is visible now. Columns where a cell went the other way (visible,
/// then hidden) are left as they are: the client has already seen them, and
/// re-hiding would cost a full column re-send for no gain.
///
/// The desired mask is recomputed per sent column from the coarse section set
/// and each visible section's cached cell graph ([`column_hidden`]); columns
/// the client holds entirely real are skipped, so a settled view costs only
/// the columns that were ever masked.
pub fn plan_reveals(
    world: &dyn BlockSource,
    visible_sections: &HashSet<(i32, i32, i32)>,
    sent: &HashMap<(i32, i32), CellMask>,
    player_section: (i32, i32, i32),
    budget: u32,
) -> Vec<(i32, i32)> {
    if budget == 0 {
        return Vec::new();
    }
    let (px, pz) = (player_section.0, player_section.2);
    let mut candidates: Vec<((i32, i32), i64)> = Vec::new();
    for (&(cx, cz), held) in sent {
        let desired = column_hidden(world, visible_sections, cx, cz);
        if !held.has_extra(&desired) {
            continue;
        }
        let dx = (cx - px) as i64;
        let dz = (cz - pz) as i64;
        candidates.push(((cx, cz), dx * dx + dz * dz));
    }
    candidates.sort_by_key(|&(_, distance)| distance);
    candidates.truncate(budget as usize);
    candidates.into_iter().map(|(column, _)| column).collect()
}

/// One per-player pass, run from the game tick.
///
/// `culling`: a search only reruns when the player crosses a `16³` section
/// border or an in-view graph changed; the search is the cheap section BFS,
/// and the per-cell mask is derived from the cached cell graphs per column
/// ([`column_hidden`]), so the tick never runs a global cell search. A reveal
/// that hit the budget keeps the plan pending so the rest of the backlog goes
/// out on following ticks.
///
/// `engine`: a block written since the last tick may expose the ore next to
/// it, so the players whose view covers it are sent that ore for real.
pub fn tick(players: &[Arc<PlayerHandle>], world: &dyn BlockSource) {
    if !crate::anti_xray_enabled()
        && players
            .iter()
            .all(|p| p.staff().xray() == crate::staff::AntiXrayLevel::Default)
    {
        return;
    }
    // A per-player `/spectate xray` override can differ from the config mode,
    // so the passes are split by each player's effective mode rather than by
    // the global one. A bypassed player and one for whom anti-xray is not in
    // force are left out entirely.
    let active: Vec<Arc<PlayerHandle>> = players
        .iter()
        .filter(|p| effective_enabled(p) && !crate::anti_xray_bypass(p))
        .cloned()
        .collect();
    let (culling, engine): (Vec<Arc<PlayerHandle>>, Vec<Arc<PlayerHandle>>) = active
        .into_iter()
        .partition(|p| effective_mode(p) == AntiXrayMode::Culling);
    if !engine.is_empty() {
        engine_tick(&engine, world);
    }
    if !culling.is_empty() {
        culling_tick(&culling, world);
    }
}

fn culling_tick(players: &[Arc<PlayerHandle>], world: &dyn BlockSource) {
    let cfg = crate::anti_xray_config();
    // Hand the sections written since the last tick to the players whose view
    // covers one, so each one's own search is invalidated — without the writer
    // thread ever walking the player list (N1). Drained here, so the set stays
    // bounded; a write that lands during this tick waits for the next one.
    let dirty: Vec<(i32, i32, i32)> = dirty_sections().lock().unwrap().drain().collect();
    if !dirty.is_empty() {
        let r = crate::server_view_radius();
        for player in players {
            if crate::anti_xray_bypass(player) {
                continue;
            }
            player.visibility().mark_dirty_for(r, &dirty);
        }
    }
    for player in players {
        if crate::anti_xray_bypass(player) {
            continue;
        }
        ensure_visible(player, world);
        let plan = player
            .visibility()
            .reveal_step(world, cfg.reveal_budget_per_tick);
        for (cx, cz) in plan {
            player.emit(&ServerEvent::ChunkColumn { cx, cz }, world);
        }
    }
}

fn engine_tick(players: &[Arc<PlayerHandle>], world: &dyn BlockSource) {
    let changed: Vec<(i32, i32, i32)> = dirty_blocks().lock().unwrap().drain().collect();
    if changed.is_empty() {
        return;
    }
    let hidden = hidden_states();
    // A player's client holds every column within the view radius at its full
    // height, so only the horizontal distance clips: a change far below or
    // above still has to be revealed. Clipping on Y used to leave those ores
    // hidden until the whole column re-streamed.
    let reach = crate::server_view_radius() * 16;
    for player in players {
        if crate::anti_xray_bypass(player) {
            continue;
        }
        let p = player.pos();
        let (px, pz) = (p.x.floor() as i32, p.z.floor() as i32);
        // Collect and remember in one lock; emit with it released, because a
        // render of the reveal may re-enter the state (culling masks the
        // single block against the reachable set).
        let to_send: Vec<(i32, i32, i32, BlockStateId)> = {
            let mut st = player.visibility();
            let mut out = Vec::new();
            for &(x, y, z) in &changed {
                if (x - px).abs() > reach || (z - pz).abs() > reach {
                    continue;
                }
                for (nx, ny, nz, block) in revealed_neighbours(world, x, y, z, hidden) {
                    // Do not re-send an ore this client already holds real.
                    if st.mark_revealed((nx, ny, nz)) {
                        out.push((nx, ny, nz, block));
                    }
                }
            }
            out
        };
        for (nx, ny, nz, block) in to_send {
            player.emit(
                &ServerEvent::BlockChange {
                    x: nx,
                    y: ny,
                    z: nz,
                    block,
                },
                world,
            );
        }
    }
}

/// The six faces of the changed cell.
const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (-1, 0, 0),
    (1, 0, 0),
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
];

/// The hidden blocks next to a cell that just changed and are now exposed, so
/// the client must be sent them for real. A hidden block that is still buried
/// stays hidden.
fn revealed_neighbours<S: BlockSource + ?Sized>(
    world: &S,
    x: i32,
    y: i32,
    z: i32,
    hidden: &HiddenSet,
) -> Vec<(i32, i32, i32, BlockStateId)> {
    let mut out = Vec::new();
    for (dx, dy, dz) in NEIGHBOURS {
        let (nx, ny, nz) = (x + dx, y + dy, z + dz);
        let block = world.block_at(nx, ny, nz);
        if hidden.contains(block) && !is_buried(world, nx, ny, nz) {
            out.push((nx, ny, nz, block));
        }
    }
    out
}

/// Whether every one of a cell's six neighbours is opaque, so the cell cannot
/// be seen without removing one. The cell itself is assumed to be a hidden
/// block (an opaque cube).
fn is_buried<S: BlockSource + ?Sized>(world: &S, x: i32, y: i32, z: i32) -> bool {
    NEIGHBOURS
        .iter()
        .all(|&(dx, dy, dz)| default_is_opaque(world.block_at(x + dx, y + dy, z + dz)))
}

/// The process-wide set of block states that engine mode hides, resolved from
/// `[anti_xray] hidden_blocks` on first use.
static HIDDEN_STATES: OnceLock<HiddenSet> = OnceLock::new();

/// The hidden-block states for the running configuration.
pub fn hidden_states() -> &'static HiddenSet {
    HIDDEN_STATES
        .get_or_init(|| build_hidden_states(&crate::anti_xray_config().hidden_block_names()))
}

/// A membership set of block states for engine mode.
///
/// Engine mode asks this once per cell of every column it sends — hundreds of
/// thousands of times per column — so the lookup has to stay in L1. The exact
/// set is a dense bitset indexed by state id, but that is ~4 KB and a random
/// access per cell. In front of it is a 512-bit "blocked" filter indexed by
/// the low 9 bits of the state: almost every cell (stone, deepslate, air) is
/// rejected by one L1-resident cache line and only a possible hit falls through
/// to the exact bitset. A hash set was measured several times slower still.
pub struct HiddenSet {
    /// `filter[state & 511]`, set for every hidden state: a false positive
    /// falls through to `bits`, a zero means definitely not hidden.
    filter: [u64; 8],
    bits: Vec<u64>,
}

impl HiddenSet {
    /// Build the set from every state of each named block. A name the registry
    /// does not know is skipped; a name with no namespace is read as
    /// `minecraft:`.
    pub fn from_names(names: &[String]) -> Self {
        let mut max = 0u32;
        let mut ids = Vec::new();
        for raw in names {
            let name = normalize_block_name(raw);
            if let Some(range) = blocks::states_of(&name) {
                for state in range {
                    max = max.max(state);
                    ids.push(state);
                }
            }
        }
        let mut bits = vec![0u64; (max as usize / 64) + 1];
        let mut filter = [0u64; 8];
        for id in ids {
            let id = id as usize;
            bits[id / 64] |= 1u64 << (id % 64);
            let lo = id & 511;
            filter[lo / 64] |= 1u64 << (lo % 64);
        }
        Self { filter, bits }
    }

    /// Whether `state` is in the set.
    #[inline]
    pub fn contains(&self, state: BlockStateId) -> bool {
        let raw = state.raw() as usize;
        let lo = raw & 511;
        // The filter is one cache line; the exact bitset only runs on a hit.
        if self.filter[lo / 64] & (1u64 << (lo % 64)) == 0 {
            return false;
        }
        self.bits
            .get(raw / 64)
            .is_some_and(|word| word & (1u64 << (raw % 64)) != 0)
    }

    /// Whether no block is hidden.
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|w| *w == 0)
    }
}

/// Resolve block names to a [`HiddenSet`]; the pure half of [`hidden_states`].
pub fn build_hidden_states(names: &[String]) -> HiddenSet {
    HiddenSet::from_names(names)
}

/// Normalise a block name: trim, lowercase, and add the `minecraft:` namespace
/// when none was written.
fn normalize_block_name(raw: &str) -> String {
    let name = raw.trim().to_ascii_lowercase();
    if name.contains(':') {
        name
    } else {
        format!("minecraft:{name}")
    }
}

/// A [`BlockSource`] for engine mode: a hidden block with all six neighbours
/// opaque is reported as the block that surrounds it.
///
/// Generic over the source it wraps, not `&dyn BlockSource`, so the inner read
/// is a direct call rather than a second virtual dispatch per cell — the codec
/// calls `block_at` for every cell twice (blocks and light), and a wrapper
/// behind a trait object would double that. The codec still takes `&dyn
/// BlockSource`; only the inner call inside the wrapper is monomorphized.
///
/// The wrapper is deliberately cheap for the common case: the hidden-set test
/// is one L1-resident bit test per cell, and the six-neighbour scan only runs
/// for a cell that is already a hidden block.
pub struct EngineColumn<'a, S: BlockSource> {
    inner: &'a S,
    hidden: &'a HiddenSet,
    masked: AtomicBool,
}

impl<'a, S: BlockSource> EngineColumn<'a, S> {
    /// Wrap `inner`, hiding the blocks in `hidden`.
    pub fn new(inner: &'a S, hidden: &'a HiddenSet) -> Self {
        Self {
            inner,
            hidden,
            masked: AtomicBool::new(false),
        }
    }

    /// Whether at least one block of the column was reported as a substitute,
    /// so `/visibility off` knows to re-send it.
    pub fn masked_any(&self) -> bool {
        self.masked.load(Ordering::Relaxed)
    }

    #[inline]
    fn substitute(&self, x: i32, y: i32, z: i32, block: BlockStateId) -> BlockStateId {
        if self.hidden.contains(block) && is_buried(self.inner, x, y, z) {
            self.masked.store(true, Ordering::Relaxed);
            surrounding_block(self.inner, self.hidden, x, y, z)
        } else {
            block
        }
    }
}

impl<S: BlockSource> BlockSource for EngineColumn<'_, S> {
    #[inline]
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        let block = self.inner.block_at(x, y, z);
        self.substitute(x, y, z, block)
    }

    fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        self.inner.copy_section(cx, cy, cz, out);
        let base_x = cx * 16;
        let base_y = cy * 16;
        let base_z = cz * 16;
        for (i, slot) in out.iter_mut().enumerate() {
            if self.hidden.contains(*slot) {
                let x = base_x + (i & 15) as i32;
                let y = base_y + (i >> 8) as i32;
                let z = base_z + ((i >> 4) & 15) as i32;
                if is_buried(self.inner, x, y, z) {
                    *slot = surrounding_block(self.inner, self.hidden, x, y, z);
                    self.masked.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    fn column_biomes(&self, cx: i32, cz: i32) -> Option<Arc<aether_worldgen::ColumnBiomes>> {
        self.inner.column_biomes(cx, cz)
    }
}

/// What an enclosed hidden block is sent as: the block that actually surrounds
/// it, so a deepslate-pocket ore becomes deepslate, a nether ore netherrack,
/// and an andesite/tuff/gravel-pocket ore that block rather than a lone stone
/// that a texture scanner would spot. The most frequent opaque non-hidden
/// neighbour wins; with none (a cluster of ores) it falls back to stone.
fn surrounding_block<S: BlockSource + ?Sized>(
    world: &S,
    hidden: &HiddenSet,
    x: i32,
    y: i32,
    z: i32,
) -> BlockStateId {
    let mut counts: Vec<(BlockStateId, u32)> = Vec::with_capacity(NEIGHBOURS.len());
    for &(dx, dy, dz) in &NEIGHBOURS {
        let b = world.block_at(x + dx, y + dy, z + dz);
        if !default_is_opaque(b) || hidden.contains(b) {
            continue;
        }
        match counts.iter_mut().find(|(s, _)| *s == b) {
            Some((_, c)) => *c += 1,
            None => counts.push((b, 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, c)| c)
        .map(|(s, _)| s)
        .unwrap_or(STONE)
}

/// A [`BlockSource`] that reports a hidden `4×4×2` cell as a single repeated
/// block.
///
/// The wrapper is what makes the anti-xray encoding codec-agnostic: every
/// version's chunk writer already turns repeated blocks into its own cheapest
/// section form, so nothing downstream has to know about visibility. The
/// atomic unit is the AVX-cell (`4×4×2`): a cell is either wholly its real
/// blocks or wholly `fill`, never split, so a partially-visible `16³` section
/// no longer leaks the ores in its hidden cells.
///
/// The mask is a per-column [`CellMask`] built by [`column_hidden`] from the
/// player's reachable section set; the mask is a plain `[u64; N]`, so no
/// `HashSet` is built or walked on the send path.
pub struct MaskedColumn<'a> {
    inner: &'a dyn BlockSource,
    cx: i32,
    cz: i32,
    hidden: CellMask,
    fill: BlockStateId,
}

impl<'a> MaskedColumn<'a> {
    /// Mask `hidden` (bit `i` is section `SECTION_MIN_Y + i`) of column
    /// `(cx, cz)` with `fill`.
    ///
    /// The section-level mask: each hidden section is expanded to its
    /// `4×4×2` cells. The wired send path uses [`Self::from_mask`] with the
    /// per-cell mask from [`column_hidden`]; this remains for callers (and the
    /// size benchmark) that hold a section mask.
    pub fn new(
        inner: &'a dyn BlockSource,
        cx: i32,
        cz: i32,
        hidden: u32,
        fill: BlockStateId,
    ) -> Self {
        let mut mask = CellMask::default();
        for i in 0..SECTION_COUNT {
            if hidden & (1 << i) == 0 {
                continue;
            }
            mask.hide_section(cx, SECTION_MIN_Y + i as i32, cz);
        }
        Self {
            inner,
            cx,
            cz,
            hidden: mask,
            fill,
        }
    }

    /// Mask exactly the given per-column cells of `(cx, cz)` with `fill`.
    pub fn from_mask(
        inner: &'a dyn BlockSource,
        cx: i32,
        cz: i32,
        hidden: CellMask,
        fill: BlockStateId,
    ) -> Self {
        Self {
            inner,
            cx,
            cz,
            hidden,
            fill,
        }
    }

    /// Mask the given world `4×4×2` cells of column `(cx, cz)` with `fill`
    /// (a convenience for tests).
    pub fn from_cells(
        inner: &'a dyn BlockSource,
        cx: i32,
        cz: i32,
        hidden_cells: impl IntoIterator<Item = (i32, i32, i32)>,
        fill: BlockStateId,
    ) -> Self {
        Self::from_mask(inner, cx, cz, CellMask::from_cells(hidden_cells), fill)
    }

    /// Whether the `4×4×2` cell holding `(x, y, z)` is hidden.
    #[inline]
    fn hides(&self, x: i32, y: i32, z: i32) -> bool {
        if x >> 4 != self.cx || z >> 4 != self.cz {
            return false;
        }
        let (cx, cy, cz) = cell_of(x, y, z);
        self.hidden.contains(cx, cy, cz)
    }
}

impl BlockSource for MaskedColumn<'_> {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        if self.hides(x, y, z) {
            return self.fill;
        }
        self.inner.block_at(x, y, z)
    }

    fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        if cx != self.cx || cz != self.cz || self.hidden.is_empty() {
            self.inner.copy_section(cx, cy, cz, out);
            return;
        }
        let dims = BoxDims::CELL;
        let (nx, ny, nz) = (CELLS_X, CELLS_Y, CELLS_Z);
        let base_x = cx * nx;
        let base_y = cy * ny;
        let base_z = cz * nz;
        // Count the hidden cells of this section first: a wholly-hidden
        // section is filled without reading the world at all, and a wholly-
        // visible one is just the world copy.
        let mut hidden = 0usize;
        for iy in 0..ny {
            for ix in 0..nx {
                for iz in 0..nz {
                    if self.hidden.contains(base_x + ix, base_y + iy, base_z + iz) {
                        hidden += 1;
                    }
                }
            }
        }
        if hidden == (nx * ny * nz) as usize {
            out.fill(self.fill);
            return;
        }
        self.inner.copy_section(cx, cy, cz, out);
        if hidden == 0 {
            return;
        }
        // Overwrite each hidden cell's blocks; a hidden cell is never split.
        for iy in 0..ny {
            for ix in 0..nx {
                for iz in 0..nz {
                    if !self.hidden.contains(base_x + ix, base_y + iy, base_z + iz) {
                        continue;
                    }
                    for by in 0..dims.y as i32 {
                        let y = iy * dims.y as i32 + by;
                        for bz in 0..dims.z as i32 {
                            let z = iz * dims.z as i32 + bz;
                            for bx in 0..dims.x as i32 {
                                let x = ix * dims.x as i32 + bx;
                                out[(x | (z << 4) | (y << 8)) as usize] = self.fill;
                            }
                        }
                    }
                }
            }
        }
    }

    fn column_biomes(&self, cx: i32, cz: i32) -> Option<Arc<aether_worldgen::ColumnBiomes>> {
        self.inner.column_biomes(cx, cz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world that is solid stone below y=0 and air above, so a section's
    /// graph is easy to reason about.
    struct FlatWorld;
    impl BlockSource for FlatWorld {
        fn block_at(&self, _x: i32, y: i32, _z: i32) -> BlockStateId {
            if y < 0 {
                STONE
            } else {
                BlockStateId::AIR
            }
        }
    }

    /// A world that is air everywhere, so every section is fully clear and
    /// every cell of it visible.
    struct AllAir;
    impl BlockSource for AllAir {
        fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
            BlockStateId::AIR
        }
    }

    /// A world with stone below `GROUND` and air above, so the section that
    /// spans `GROUND` is only partly visible — the case cell culling fixes.
    struct GroundWorld {
        ground: i32,
    }
    impl BlockSource for GroundWorld {
        fn block_at(&self, _x: i32, y: i32, _z: i32) -> BlockStateId {
            if y < self.ground {
                STONE
            } else {
                BlockStateId::AIR
            }
        }
    }

    /// Every section of `columns`, at every world height.
    fn all_sections(columns: &[(i32, i32)]) -> HashSet<(i32, i32, i32)> {
        let mut vis = HashSet::new();
        for &(cx, cz) in columns {
            for cy in SECTION_MIN_Y..=SECTION_MAX_Y {
                vis.insert((cx, cy, cz));
            }
        }
        vis
    }

    /// A state whose coarse reachable set is every section of `columns`.
    fn state_showing(columns: &[(i32, i32)], search_id: u64) -> State {
        State {
            center: Some((0, 0, 0)),
            dirty: false,
            search_id,
            visible_sections: all_sections(columns),
            own_cell: None,
            sent: HashMap::new(),
            planned_search: search_id,
            reveal_pending: false,
            bypass: false,
            revealed: HashSet::new(),
        }
    }

    /// Mark every cell of `(cx, cz)` hidden in what the client holds.
    fn sent_full(st: &mut State, cx: i32, cz: i32) {
        st.sent.insert((cx, cz), CellMask::full());
    }

    #[test]
    fn a_fully_visible_column_has_no_hidden_cells() {
        let _guard = global_test_lock();
        invalidate_all();
        let hidden = column_hidden(&AllAir, &all_sections(&[(0, 0)]), 0, 0);
        assert!(hidden.is_empty());
    }

    #[test]
    fn a_lone_sealed_section_of_a_column_is_hidden_whole() {
        // A column whose only visible section is one of them: every other
        // section's cells stay hidden. The section-level rule that a fully
        // opaque section is never visible is what keeps this from leaking.
        let _guard = global_test_lock();
        invalidate_all();
        let mut visible = HashSet::new();
        visible.insert((0, 4, 0));
        let hidden = column_hidden(&GroundWorld { ground: 8 }, &visible, 0, 0);
        // The visible section's sky cells are real; everything else stone.
        assert!(
            !hidden.contains(0, 35, 0),
            "the visible section's air is real"
        );
        assert!(
            hidden.contains(0, -30, 0),
            "a section not in the set is hidden"
        );
    }

    #[test]
    fn a_reveal_plan_cut_short_by_the_budget_continues_next_tick() {
        // Every column is sent fully hidden and every cell is now visible:
        // six columns, two per tick. All six must go out across ticks, not
        // just the first two (finding 1).
        let _guard = global_test_lock();
        invalidate_all();
        let columns = [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0)];
        let mut st = state_showing(&columns, 1);
        // The masks are stale relative to the fresh reachable set, so a plan
        // is due (the planner has not run for this search id yet).
        st.planned_search = 0;
        for &col in &columns {
            sent_full(&mut st, col.0, col.1);
        }
        let mut revealed = Vec::new();
        let mut ticks = 0;
        loop {
            let plan = st.reveal_step(&AllAir, 2);
            if plan.is_empty() {
                break;
            }
            ticks += 1;
            assert!(ticks < 100, "the reveal must make progress");
            for (cx, cz) in plan {
                let hidden = st.hidden_for(&AllAir, cx, cz);
                st.record_sent(cx, cz, hidden);
                revealed.push((cx, cz));
            }
        }
        revealed.sort();
        let mut expected = columns.to_vec();
        expected.sort();
        assert_eq!(revealed, expected, "every column must be re-sent");
        assert!(ticks > 1, "the budget must force more than one tick");
    }

    #[test]
    fn the_reveal_plan_is_nearest_first_and_budgeted() {
        let _guard = global_test_lock();
        invalidate_all();
        let columns = [(0, 0), (1, 0), (2, 0), (5, 0), (10, 0)];
        let vis = all_sections(&columns);
        let mut sent = HashMap::new();
        for &col in &columns {
            sent.insert(col, CellMask::full());
        }
        let plan = plan_reveals(&AllAir, &vis, &sent, (0, 0, 0), 2);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], (0, 0));
        assert!(plan.contains(&(1, 0)), "nearest first: {plan:?}");
    }

    #[test]
    fn no_reveal_when_nothing_became_visible() {
        let _guard = global_test_lock();
        invalidate_all();
        let columns = [(0, 0), (1, 0)];
        let vis = all_sections(&columns);
        let mut sent = HashMap::new();
        sent.insert((0, 0), CellMask::default());
        sent.insert((1, 0), CellMask::default());
        assert!(plan_reveals(&AllAir, &vis, &sent, (0, 0, 0), 32).is_empty());
    }

    #[test]
    fn an_idle_player_does_not_replan() {
        let _guard = global_test_lock();
        invalidate_all();
        let columns = [(0, 0), (1, 0)];
        let mut st = state_showing(&columns, 7);
        for &col in &columns {
            st.sent.insert(col, CellMask::default());
        }
        assert!(st.reveal_step(&AllAir, 32).is_empty());
        // Still nothing a second time.
        assert!(st.reveal_step(&AllAir, 32).is_empty());
    }

    #[test]
    fn a_respawned_client_forgets_its_old_columns() {
        // Old columns must not stay in `sent` forever (finding 7).
        let mut st = State {
            center: Some((0, 0, 0)),
            search_id: 5,
            planned_search: 5,
            ..Default::default()
        };
        sent_full(&mut st, 0, 0);
        sent_full(&mut st, 1, 0);
        st.reset();
        assert!(st.sent().is_empty(), "reset drops the old columns");
        assert!(st.visible().is_empty());
        assert!(st.reveal_step(&AllAir, 32).is_empty());
    }

    #[test]
    fn a_reveal_is_sent_once_and_reset_on_unload() {
        // Engine mode must not re-send the same ore every time a neighbour
        // changes, and must forget it when the column is streamed again.
        let mut st = State::default();
        assert!(st.mark_revealed((8, -20, 8)), "first reveal is new");
        assert!(
            !st.mark_revealed((8, -20, 8)),
            "the same cell is not re-sent"
        );
        assert!(st.is_revealed(&(8, -20, 8)));

        // A different cell in the same column is remembered too.
        assert!(st.mark_revealed((8, -30, 8)));

        // Unloading the column forgets both, so a re-stream can reveal again.
        st.forget(0, 0);
        assert!(!st.is_revealed(&(8, -20, 8)));
        assert!(!st.is_revealed(&(8, -30, 8)));
        assert!(st.mark_revealed((8, -20, 8)));

        // A reveal in another column survives this column's unload.
        assert!(st.mark_revealed((40, -20, 40)));
        st.forget(0, 0);
        assert!(st.is_revealed(&(40, -20, 40)));
    }

    #[test]
    fn a_write_dirties_only_players_whose_view_covers_it() {
        // A break inside a player's view marks them; one on the far side of
        // the map does not (finding 5).
        let mut near = State {
            center: Some((0, 0, 0)),
            ..Default::default()
        };
        near.mark_dirty_for(8, &[(3, 100, 3)]);
        assert!(near.dirty, "a change in the view box must mark the player");
        assert!(!near.is_fresh((0, 0, 0)));

        let mut far = State {
            center: Some((0, 0, 0)),
            ..Default::default()
        };
        far.mark_dirty_for(8, &[(20, 0, 0)]);
        assert!(!far.dirty, "a change beyond the view box must not");
    }

    #[test]
    fn a_graph_computed_before_a_write_is_never_cached() {
        // Round-3 New-1, the exact interleaving: two computes B then A of the
        // same section plus a write W. B reads the version and snapshots
        // pre-write; W bumps the version; A reads the new version, snapshots
        // post-write and stores; B then tries to store its stale graph. With a
        // monotonic global version B's store is rejected — the per-section
        // reset that let `0 == unwrap_or(0)` match again is gone.
        let _guard = global_test_lock();
        invalidate_all();
        let key = (0, 2, 0);
        let mut cc = GraphCache::default();

        // B: misses at the version current when it started.
        let b_version = EDIT_EPOCH.load(Ordering::Relaxed);

        // W: a write lands (evicts and bumps).
        invalidate_section(&mut cc, key);

        // A: reads the new version, computes post-write, stores.
        let a_version = EDIT_EPOCH.load(Ordering::Relaxed);
        assert_ne!(a_version, b_version, "the write advanced the version");
        store_section_cells(
            &mut cc,
            key,
            Arc::new(compute_section_cells(&[STONE; 4096])),
            a_version,
        );
        assert!(cc.sections.contains_key(&key), "A's fresh graph is cached");

        // B: stores its stale pre-write graph with the version it read.
        store_section_cells(
            &mut cc,
            key,
            Arc::new(compute_section_cells(&[BlockStateId::AIR; 4096])),
            b_version,
        );
        assert_eq!(
            cc.sections.get(&key).map(|c| c.section),
            Some(SectionGraph::sealed()),
            "B's stale graph must not overwrite A's"
        );
    }

    /// A world whose section snapshot simulates a concurrent block write
    /// arriving while the graph is being computed.
    struct WriteDuringSnapshot;
    impl BlockSource for WriteDuringSnapshot {
        fn block_at(&self, _x: i32, _y: i32, _z: i32) -> BlockStateId {
            BlockStateId::AIR
        }
        fn copy_section(&self, cx: i32, cy: i32, cz: i32, _out: &mut [BlockStateId; 4096]) {
            record_invalidation((cx, cy, cz));
        }
    }

    #[test]
    fn a_write_during_a_graph_snapshot_is_not_cached() {
        // The reviewer's requested concurrency test: invalidate_block
        // interleaved with an in-flight graph computation, driven through the
        // real `graph()` (not its pieces). The store must reject the graph
        // computed from the pre-write snapshot.
        let _guard = global_test_lock();
        invalidate_all();
        let key = (0, 2, 0);
        assert!(section_cells(&WriteDuringSnapshot, key).is_some());
        assert!(
            !cache().lock().unwrap().sections.contains_key(&key),
            "graphs whose snapshot raced a write must not be cached"
        );
        // The same call without a racing write caches it, and a later write
        // evicts it.
        assert!(section_cells(&FlatWorld, key).is_some());
        assert!(
            cache().lock().unwrap().sections.contains_key(&key),
            "an un-raced graph is cached"
        );
        record_invalidation(key);
        assert!(!cache().lock().unwrap().sections.contains_key(&key));
    }

    /// Serialises the tests that touch the process-wide cache, epoch and
    /// dirty set, which are shared across the whole test binary.
    fn global_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn write_invalidation_is_free_when_off_and_records_when_on() {
        // N1: with culling off (the default, and what every test here runs
        // under) a block write must not touch the cache or record a section.
        // Once on, it records the section and evicts the graph.
        let _guard = global_test_lock();
        invalidate_all();
        section_cells(&FlatWorld, (0, 2, 0));
        invalidate_block(0, 32, 0);
        assert!(
            dirty_sections().lock().unwrap().is_empty(),
            "a disabled write must not record a section"
        );
        assert!(
            cache().lock().unwrap().sections.contains_key(&(0, 2, 0)),
            "a disabled write must not evict a cached graph"
        );

        // The enabled path (what `invalidate_block` runs once the flag is on).
        record_invalidation((0, 2, 0));
        assert!(!cache().lock().unwrap().sections.contains_key(&(0, 2, 0)));
        assert!(dirty_sections().lock().unwrap().contains(&(0, 2, 0)));
        dirty_sections().lock().unwrap().clear();
    }

    #[test]
    fn a_write_during_the_search_keeps_the_reachable_set_stale() {
        // N2 / the residual race of finding 4: the search reads the edit epoch
        // before it starts and only clears the stale flag if no write landed
        // while it ran.
        let _guard = global_test_lock();
        let mut st = State {
            center: Some((0, 0, 0)),
            dirty: true,
            ..Default::default()
        };
        let epoch_before = EDIT_EPOCH.load(Ordering::Relaxed);
        EDIT_EPOCH.fetch_add(1, Ordering::Relaxed); // a write lands mid-search
        st.set_visible_if((0, 0, 0), HashSet::new(), epoch_before);
        assert!(st.dirty, "a mid-search write must not be cleared");
        assert!(!st.is_fresh((0, 0, 0)));

        // With no write during the search the flag clears.
        let epoch_before = EDIT_EPOCH.load(Ordering::Relaxed);
        st.set_visible_if((0, 0, 0), HashSet::new(), epoch_before);
        assert!(!st.dirty);
        assert!(st.is_fresh((0, 0, 0)));
    }

    #[test]
    fn reveal_all_selects_only_columns_still_masked() {
        // N4: `/visibility off` must re-send the columns holding stone, and
        // leave the unmasked ones alone.
        let mut sent = HashMap::new();
        sent.insert((0, 0), CellMask::default());
        sent.insert((1, 0), CellMask::full());
        sent.insert((2, 0), CellMask::from_cells([(4, 0, 4)]));
        let mut got = masked_columns(&sent);
        got.sort();
        assert_eq!(got, vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn rendering_a_bypassed_column_records_a_zero_mask() {
        let _guard = global_test_lock();
        invalidate_all();
        // N4 / New-2: `/visibility off` re-renders the masked columns; each
        // must record an empty mask, or the planner keeps believing the client
        // holds stone and re-plans columns it already has.
        let columns = [(0, 0), (1, 0)];
        let mut st = state_showing(&columns, 1);
        for &col in &columns {
            sent_full(&mut st, col.0, col.1);
        }
        assert_eq!(masked_columns(st.sent()).len(), 2, "both start masked");

        // The reveal pass re-renders each column with bypass on.
        for (cx, cz) in masked_columns(st.sent()) {
            assert!(st.mask_for_render(&AllAir, cx, cz, true).is_empty());
        }
        assert!(
            masked_columns(st.sent()).is_empty(),
            "after reveal the client holds real columns and `sent` says so"
        );

        // A non-bypassed render still records the real mask: a column the
        // player cannot see comes back with every cell hidden.
        let hidden = st.mask_for_render(&AllAir, 9, 9, false);
        assert_eq!(hidden.len(), CELLS_PER_COLUMN, "every cell hidden");
    }

    #[test]
    fn the_tick_hands_only_in_range_sections_to_a_player() {
        // A drained section within the player's view dirties them; one beyond
        // it does not, and the drain is what bounds the set.
        let mut st = State {
            center: Some((0, 0, 0)),
            ..Default::default()
        };
        st.mark_dirty_for(2, &[(5, 0, 0)]);
        assert!(!st.dirty, "5 sections away is outside a radius-2 view");
        st.mark_dirty_for(2, &[(1, 0, 1)]);
        assert!(st.dirty, "1 section away is inside");
    }

    #[test]
    fn masked_column_reports_the_fill_in_a_hidden_section_only() {
        let world = FlatWorld;
        let hide_surface = 1u32 << ((0 - SECTION_MIN_Y) as u32);
        let masked = MaskedColumn::new(&world, 0, 0, hide_surface, STONE);
        assert_eq!(masked.block_at(0, 5, 0), STONE, "hidden air becomes stone");
        assert_eq!(masked.block_at(0, -5, 0), STONE);

        let other = MaskedColumn::new(&world, 1, 0, hide_surface, STONE);
        assert_eq!(
            other.block_at(0, 5, 0),
            BlockStateId::AIR,
            "other column real"
        );
    }

    #[test]
    fn re_rendering_a_column_keeps_the_mask_the_client_holds() {
        let _guard = global_test_lock();
        invalidate_all();
        // A column is rendered many times over a session (join, streaming,
        // reveal). Every render must compute the mask from the current
        // reachable set and record it, so a re-send cannot drop or add
        // masking. An empty reachable set hides every cell.
        let mut st = State::default();
        let first = st.mask_for_render(&AllAir, 0, 0, false);
        assert_eq!(first.len(), CELLS_PER_COLUMN, "all cells hidden");
        let second = st.mask_for_render(&AllAir, 0, 0, false);
        assert_eq!(first, second, "a second render is identical");
        assert!(!st.sent().get(&(0, 0)).unwrap().is_empty());

        // Once the sections are visible, the same column renders unmasked,
        // and `sent` records that the client now holds the real column.
        let all = all_sections(&[(0, 0)]);
        st.set_visible_if((0, 0, 0), all, EDIT_EPOCH.load(Ordering::Relaxed));
        assert!(st.mask_for_render(&AllAir, 0, 0, false).is_empty());
        assert!(!st.sent().contains_key(&(0, 0)));
    }

    #[test]
    fn a_flat_world_below_the_surface_caches_a_sealed_section_graph() {
        let _guard = global_test_lock();
        invalidate_all();
        let world = FlatWorld;
        // A wholly stone section: sealed, no cell visible.
        let cells = section_cells(&world, (0, -2, 0)).unwrap();
        assert_eq!(cells.section.open_faces(), 0);
        assert_eq!(cells.visible, 0);
        // A wholly air section: fully open, every cell visible.
        let cells = section_cells(&world, (0, 2, 0)).unwrap();
        assert_eq!(
            cells.section.open_faces(),
            aether_world::visibility::ALL_FACES
        );
        assert_eq!(cells.visible, u128::MAX);
    }

    #[test]
    fn a_masked_column_is_whole_sections_stone_or_whole_sections_real() {
        // The culling invariant: a hidden section is *entirely* the fill, a
        // visible section is *entirely* its real blocks. No selective
        // replacement of individual blocks inside a hidden section.
        struct Distinct;
        impl BlockSource for Distinct {
            fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
                // Never AIR or STONE, and different in every cell, so a
                // partial replacement would show up as a mismatch.
                let h = (x as u32).wrapping_mul(2_654_435_761)
                    ^ (y as u32).wrapping_mul(40_503)
                    ^ (z as u32).wrapping_mul(22_682_429);
                BlockStateId(h | 0x8000_0000)
            }
        }
        let world = Distinct;
        let hidden = (1 << 5) | (1 << 8); // two hidden sections, one visible between
        let masked = MaskedColumn::new(&world, 0, 0, hidden, STONE);
        let mut out = [BlockStateId::AIR; 4096];
        for i in 0..SECTION_COUNT {
            let cy = SECTION_MIN_Y + i as i32;
            let is_hidden = hidden & (1u32 << i as u32) != 0;
            masked.copy_section(0, cy, 0, &mut out);
            if is_hidden {
                assert!(
                    out.iter().all(|b| *b == STONE),
                    "hidden section {cy} must be entirely stone"
                );
            } else {
                for (j, got) in out.iter().enumerate() {
                    let x = (j & 15) as i32;
                    let z = ((j >> 4) & 15) as i32;
                    let y = (j >> 8) as i32;
                    let want = world.block_at(x, cy * 16 + y, z);
                    assert_eq!(*got, want, "visible section {cy} must be untouched");
                }
            }
        }
    }

    // ---- AX3b: per-cell (4×4×2) mask --------------------------------

    /// A world where every block is unique, so a partial replacement of a
    /// cell shows up as a mismatch.
    struct UniqueWorld;
    impl BlockSource for UniqueWorld {
        fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
            let h = (x as u32).wrapping_mul(2_654_435_761)
                ^ (y as u32).wrapping_mul(40_503)
                ^ (z as u32).wrapping_mul(22_682_429);
            BlockStateId(h | 0x8000_0000)
        }
    }

    /// The `(nx, ny, nz)` cells along each axis of a `16³` section.
    fn test_grid() -> (i32, i32, i32) {
        (
            (SECTION_SIZE as usize / BoxDims::CELL.x) as i32,
            (SECTION_SIZE as usize / BoxDims::CELL.y) as i32,
            (SECTION_SIZE as usize / BoxDims::CELL.z) as i32,
        )
    }

    #[test]
    fn masked_column_hides_exactly_the_requested_cells() {
        // Pass-after: a per-cell mask hides the requested 4×4×2 cells and
        // leaves every other block byte-identical to the source. Fail-before
        // (`from_cells`/cell mask absent) the section-only wrapper could not
        // express a partly-hidden section at all.
        let world = UniqueWorld;
        let hidden: HashSet<(i32, i32, i32)> = [(0, 0, 1), (3, 7, 2), (1, -25, 3), (2, 8, 0)]
            .into_iter()
            .collect();
        let masked = MaskedColumn::from_cells(&world, 0, 0, hidden.clone(), STONE);
        let mut out = [BlockStateId::AIR; 4096];
        let mut hidden_hits = 0usize;
        for cy in SECTION_MIN_Y..=SECTION_MAX_Y {
            masked.copy_section(0, cy, 0, &mut out);
            for (i, &got) in out.iter().enumerate() {
                let x = (i & 15) as i32;
                let z = ((i >> 4) & 15) as i32;
                let y = cy * 16 + (i >> 8) as i32;
                let want = if hidden.contains(&cell_of(x, y, z)) {
                    hidden_hits += 1;
                    STONE
                } else {
                    world.block_at(x, y, z)
                };
                assert_eq!(got, want, "copy_section block ({x},{y},{z})");
                assert_eq!(masked.block_at(x, y, z), want, "block_at ({x},{y},{z})");
            }
        }
        // Every requested cell really was exercised.
        assert_eq!(
            hidden_hits,
            hidden.len() * 32,
            "each hidden cell is 32 blocks"
        );
    }

    #[test]
    fn a_hidden_cell_is_whole_fill_and_a_visible_cell_is_whole_real() {
        // Pass-after: the whole-cell invariant — a hidden cell is entirely
        // `fill`, a visible cell entirely its real blocks, on `copy_section`
        // and `block_at` alike, and a re-encoding is byte-for-byte identical.
        let world = UniqueWorld;
        let hidden: HashSet<(i32, i32, i32)> = [(1, 0, 1), (2, 1, 3)].into_iter().collect();
        let masked = MaskedColumn::from_cells(&world, 0, 0, hidden.clone(), STONE);
        let mut first = [BlockStateId::AIR; 4096];
        let mut second = [BlockStateId::AIR; 4096];
        masked.copy_section(0, 0, 0, &mut first);
        masked.copy_section(0, 0, 0, &mut second);
        assert_eq!(first, second, "re-encoding the same column is stable");
        for (i, &got) in first.iter().enumerate() {
            let x = (i & 15) as i32;
            let z = ((i >> 4) & 15) as i32;
            let y = (i >> 8) as i32;
            let cell = cell_of(x, y, z);
            if hidden.contains(&cell) {
                assert_eq!(got, STONE, "hidden cell {cell:?} must be wholly fill");
                assert_eq!(masked.block_at(x, y, z), STONE);
            } else {
                assert_eq!(
                    got,
                    world.block_at(x, y, z),
                    "visible cell {cell:?} must be wholly real"
                );
                assert_eq!(masked.block_at(x, y, z), world.block_at(x, y, z));
            }
        }
    }

    #[test]
    fn a_partly_visible_section_sends_the_ore_in_a_hidden_cell_as_fill() {
        // The owner's leak (fail-before/pass-after): a 16³ section with one
        // visible 4×4×2 cell and the rest hidden. The visible cell is real,
        // every hidden cell is `fill`, so the ore in a hidden cell never
        // reaches the client.
        struct PartialOre;
        impl BlockSource for PartialOre {
            fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
                match cell_of(x, y, z) {
                    (0, 0, 0) => BlockStateId(1234), // the ore, in a hidden cell
                    (1, 0, 0) => BlockStateId(5678), // the one visible cell
                    _ => STONE,
                }
            }
        }
        let world = PartialOre;
        let (nx, ny, nz) = test_grid();
        let visible_cell = (1, 0, 0);
        let mut hidden = HashSet::new();
        for iy in 0..ny {
            for ix in 0..nx {
                for iz in 0..nz {
                    let cell = (ix, iy, iz);
                    if cell != visible_cell {
                        hidden.insert(cell);
                    }
                }
            }
        }
        let masked = MaskedColumn::from_cells(&world, 0, 0, hidden, STONE);
        // The ore sits in the hidden cell (0,0,0): both paths must send fill.
        assert_eq!(masked.block_at(0, 0, 0), STONE, "the hidden ore is stone");
        assert_eq!(masked.block_at(1, 1, 1), STONE);
        // The one visible cell stays real.
        assert_eq!(masked.block_at(4, 0, 0), BlockStateId(5678));
        let mut out = [BlockStateId::AIR; 4096];
        masked.copy_section(0, 0, 0, &mut out);
        assert_eq!(out[0], STONE, "copy_section hides the ore cell");
        assert_eq!(
            out[4],
            BlockStateId(5678),
            "copy_section keeps the visible cell"
        );
        // Every other cell of the section is wholly fill too.
        for (i, &got) in out.iter().enumerate() {
            let x = (i & 15) as i32;
            let z = ((i >> 4) & 15) as i32;
            let y = (i >> 8) as i32;
            if cell_of(x, y, z) == visible_cell {
                continue;
            }
            assert_eq!(got, STONE, "hidden block ({x},{y},{z}) must be fill");
        }
    }

    #[test]
    fn a_partly_visible_surface_section_is_masked_per_cell() {
        // The owner's leak: a section with air on top and stone below is only
        // partly visible. The cell mask must keep the sky real and the rock
        // hidden — the section-level mask would send the whole section real.
        let _guard = global_test_lock();
        invalidate_all();
        let world = GroundWorld { ground: 8 };
        let vis = visible_from_margin(&world, (0.5, 20.5, 0.5), 1, 2);
        let hidden = column_hidden(&world, &vis, 0, 0);
        assert!(
            !hidden.contains(0, 5, 0),
            "open sky above the ground is real ([0,5,0] is cell y=10)"
        );
        assert!(
            !hidden.contains(0, 3, 0),
            "the surface top-rock the player can see is real (block y=6)"
        );
        assert!(
            hidden.contains(0, 1, 0),
            "rock below the surface is hidden (block y=2)"
        );
        assert!(hidden.contains(0, -30, 0), "the deep underground is hidden");
    }

    #[test]
    fn the_section_search_covers_the_whole_view_distance() {
        // Blocker 1 regression: `view_distance` is in sections, not cells, so
        // an all-air view reaches `view_distance` * 16 blocks, not a quarter
        // of it. A section 2 chunks out is visible; one past the radius is not.
        let _guard = global_test_lock();
        invalidate_all();
        let vis = visible_from_margin(&AllAir, (0.5, 5.5, 0.5), 2, 2);
        assert!(
            vis.contains(&(2, 0, 0)),
            "2 sections (32 blocks) is in view"
        );
        assert!(!vis.contains(&(3, 0, 0)), "past the radius is not");
    }

    #[test]
    fn the_section_search_reaches_the_surface_and_not_the_deep_stone() {
        let _guard = global_test_lock();
        invalidate_all();
        let vis = visible_from_margin(&FlatWorld, (0.5, 5.5, 0.5), 1, 2);
        assert!(vis.contains(&(0, 0, 0)), "the surface air is visible");
        assert!(!vis.contains(&(0, -1, 0)), "the deep stone stays hidden");
    }

    #[test]
    fn the_spawn_column_is_masked_the_way_the_join_path_masks_it() {
        // The join path masks its first columns through exactly these calls
        // (finding 2). A buried ore below the surface must not reach the
        // client as itself.
        let _guard = global_test_lock();
        invalidate_all();
        struct BuriedOre;
        impl BlockSource for BuriedOre {
            fn block_at(&self, _x: i32, y: i32, _z: i32) -> BlockStateId {
                if y == -48 {
                    BlockStateId(1234) // a stand-in ore id
                } else if y < 0 {
                    STONE
                } else {
                    BlockStateId::AIR
                }
            }
        }
        let world = BuriedOre;
        let vis = visible_from_margin(&world, (0.5, 5.5, 0.5), 2, 2);
        let hidden = column_hidden(&world, &vis, 0, 0);
        assert!(
            hidden.contains(0, -24, 0),
            "the deep ore cell must be hidden"
        );
        let masked =
            MaskedColumn::from_mask(&world, 0, 0, hidden, fill_for_mode(GameMode::Creative));
        assert_eq!(
            masked.block_at(0, -48, 0),
            STONE,
            "the buried ore must be sent as stone"
        );
        // A column with no mask keeps the real block.
        let unmasked = MaskedColumn::from_mask(&world, 1, 0, CellMask::default(), STONE);
        assert_eq!(unmasked.block_at(0, -48, 0), BlockStateId(1234));
    }

    #[test]
    fn cell_of_floors_negative_coordinates() {
        let pos = crate::players::PosLook {
            x: -0.5,
            y: -1.0,
            z: 15.9,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        };
        assert_eq!(cell_of_pos(pos), (-1, -1, 3));
    }

    #[test]
    fn the_players_own_sealed_pocket_is_not_encased() {
        // A player sealed in a one-block air pocket in solid rock: the pocket
        // is interior, so the section mask hides it — but the player's own
        // cell must be revealed, or the client would encase them in stone.
        let _guard = global_test_lock();
        invalidate_all();
        struct Pocket;
        impl BlockSource for Pocket {
            fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
                if (x, y, z) == (8, 8, 8) {
                    BlockStateId::AIR
                } else {
                    STONE
                }
            }
        }
        let mut st = State::default();
        let mut sections = HashSet::new();
        sections.insert((0, 0, 0));
        st.set_visible_if((0, 0, 0), sections, EDIT_EPOCH.load(Ordering::Relaxed));
        st.set_own_cell(cell_of(8, 8, 8));
        let mask = st.hidden_for(&Pocket, 0, 0);
        assert!(
            !mask.contains(2, 4, 2),
            "the player's own pocket cell must not be masked"
        );
        assert!(
            mask.contains(2, 1, 2),
            "a buried cell of the same section stays hidden"
        );
    }

    #[test]
    fn the_reachable_set_seed_is_quantised_to_the_section() {
        // The performance bound: two positions inside the same `16³` section
        // share the freshness key, so moving a cell within it reuses the
        // previous (cheap section) search instead of re-running it.
        let a = crate::players::PosLook {
            x: 1.0,
            y: 5.0,
            z: 1.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        };
        let b = crate::players::PosLook {
            x: 14.0,
            y: 14.0,
            z: 14.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        };
        assert_eq!(section_of(a), section_of(b));
        // A move across the border is a new key.
        let c = crate::players::PosLook { x: 16.5, ..a };
        assert_ne!(section_of(a), section_of(c));
    }

    // ---- engine mode ----------------------------------------------------

    fn ore(name: &str) -> BlockStateId {
        blocks::default_state(name).expect("engine registry has the ore")
    }

    /// A world with one ore cell, the six cells touching it filled with
    /// `around` (or air on the +X face when `open`, so the ore is exposed),
    /// and air everywhere else.
    struct OreWorld {
        ore: BlockStateId,
        at: (i32, i32, i32),
        open: bool,
        around: BlockStateId,
    }

    impl BlockSource for OreWorld {
        fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
            let (ox, oy, oz) = self.at;
            if (x, y, z) == self.at {
                self.ore
            } else if self.open && (x, y, z) == (ox + 1, oy, oz) {
                BlockStateId::AIR
            } else if (y - oy).abs() <= 1 && (x - ox).abs() <= 1 && (z - oz).abs() <= 1 {
                self.around
            } else {
                BlockStateId::AIR
            }
        }
    }

    fn hidden_from(names: &[&str]) -> HiddenSet {
        build_hidden_states(&names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn hidden_states_resolve_names_with_and_without_a_namespace() {
        let set = hidden_from(&["diamond_ore", "minecraft:deepslate_diamond_ore"]);
        assert!(set.contains(ore("minecraft:diamond_ore")));
        assert!(set.contains(ore("minecraft:deepslate_diamond_ore")));
        assert!(!set.contains(ore("minecraft:coal_ore")));
        assert!(build_hidden_states(&["minecraft:not_a_real_block".into()]).is_empty());
    }

    #[test]
    fn an_enclosed_ore_is_sent_as_the_surrounding_block() {
        let hidden = hidden_from(&["diamond_ore"]);
        let world = OreWorld {
            ore: ore("minecraft:diamond_ore"),
            at: (8, -20, 8),
            open: false,
            around: ore("minecraft:deepslate"),
        };
        let column = EngineColumn::new(&world, &hidden);
        assert!(is_buried(&world, 8, -20, 8));
        // The substitute is the block actually around it, not a guess from y.
        assert_eq!(column.block_at(8, -20, 8), ore("minecraft:deepslate"));
        assert!(column.masked_any(), "the caller must know to re-send");
        // The rock beside it is untouched.
        assert_eq!(column.block_at(9, -20, 8), ore("minecraft:deepslate"));
    }

    #[test]
    fn a_stone_pocket_ore_is_sent_as_stone() {
        let hidden = hidden_from(&["diamond_ore"]);
        let world = OreWorld {
            ore: ore("minecraft:diamond_ore"),
            at: (8, 20, 8),
            open: false,
            around: STONE,
        };
        let column = EngineColumn::new(&world, &hidden);
        assert_eq!(column.block_at(8, 20, 8), STONE);
    }

    #[test]
    fn a_nether_ore_is_sent_as_netherrack() {
        let hidden = hidden_from(&["nether_quartz_ore"]);
        let world = OreWorld {
            ore: ore("minecraft:nether_quartz_ore"),
            at: (8, 40, 8),
            open: false,
            around: ore("minecraft:netherrack"),
        };
        let column = EngineColumn::new(&world, &hidden);
        assert_eq!(column.block_at(8, 40, 8), ore("minecraft:netherrack"));
    }

    #[test]
    fn an_exposed_ore_is_sent_as_itself() {
        let hidden = hidden_from(&["diamond_ore"]);
        let world = OreWorld {
            ore: ore("minecraft:diamond_ore"),
            at: (8, -20, 8),
            open: true,
            around: ore("minecraft:deepslate"),
        };
        let column = EngineColumn::new(&world, &hidden);
        assert!(!is_buried(&world, 8, -20, 8));
        assert_eq!(column.block_at(8, -20, 8), ore("minecraft:diamond_ore"));
        assert!(!column.masked_any(), "nothing was hidden");
    }

    #[test]
    fn breaking_a_neighbour_reveals_a_buried_ore() {
        // The ore is enclosed here, so nothing is revealed.
        let hidden = hidden_from(&["diamond_ore"]);
        let buried = OreWorld {
            ore: ore("minecraft:diamond_ore"),
            at: (8, -20, 8),
            open: false,
            around: ore("minecraft:deepslate"),
        };
        assert!(revealed_neighbours(&buried, 9, -20, 8, &hidden).is_empty());

        // The block beside it is gone: the ore is now visible and must be sent.
        let open = OreWorld {
            ore: ore("minecraft:diamond_ore"),
            at: (8, -20, 8),
            open: true,
            around: ore("minecraft:deepslate"),
        };
        let revealed = revealed_neighbours(&open, 9, -20, 8, &hidden);
        assert_eq!(
            revealed,
            vec![(8, -20, 8, ore("minecraft:diamond_ore"))],
            "the now-exposed ore must be re-sent"
        );

        // Ordinary stone is never revealed.
        assert!(revealed_neighbours(&open, 9, -20, 9, &hidden).is_empty());
    }

    // ---- culling regressions (live bug AX2) -----------------------------

    /// A deep, otherwise uniform stone world with air above y=0 and optional
    /// ore and water/water-cave cells, for the culling regression tests.
    struct DeepGround {
        ore: BlockStateId,
        ore_at: Option<(i32, i32, i32)>,
    }

    impl BlockSource for DeepGround {
        fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
            if Some((x, y, z)) == self.ore_at {
                return self.ore;
            }
            if y >= 0 {
                return BlockStateId::AIR;
            }
            STONE
        }
    }

    #[test]
    fn a_far_buried_ore_behind_solid_stone_is_hidden() {
        // The player is on the surface; a far column is solid stone with an
        // ore under the surface (the "shell" beside open air) and another
        // deeper. Both stay hidden because their section is fully opaque, so
        // the coarse search never marks it visible and `column_hidden` leaves
        // every cell of it as `fill` — the ore cannot be sent real.
        //
        // Note this is a *section-level* guarantee, not the `adj` cell rule:
        // `compute_cell_visible` deliberately marks an opaque cell visible when
        // it borders a visible one (so the surface dirt/water the player looks
        // at is real). A sealed section has no visible cell in it to seed that,
        // so the ore is hidden. The cell-level rule is exercised by
        // `a_partly_visible_surface_section_is_masked_per_cell`.
        let _guard = global_test_lock();
        invalidate_all();
        let world = DeepGround {
            ore: ore("minecraft:coal_ore"),
            ore_at: Some((40, -5, 0)),
        };
        let vis = visible_from_margin(&world, (0.5, 5.5, 0.5), 2, 2);
        // x=40 is column 2, within the view.
        assert!(
            !vis.contains(&(2, -1, 0)),
            "the fully opaque ore section is not reachable"
        );
        let hidden = column_hidden(&world, &vis, 2, 0);
        assert!(
            hidden.contains(10, -3, 0),
            "the shell ore cell is hidden (block y=-5)"
        );
        assert!(
            hidden.contains(10, -10, 0),
            "a deep stone cell is hidden (block y=-20)"
        );
        // The air above the surface is still real.
        let hidden0 = column_hidden(&world, &vis, 0, 0);
        assert!(
            !hidden0.contains(0, 3, 0),
            "open-air surface is visible (block y=5)"
        );
    }

    #[test]
    fn an_unseen_water_cave_is_hidden() {
        // A water shaft from the surface down into a flooded cave the player
        // cannot see. Water is see-through, so the shaft is *visible*, but it
        // must not let the search walk into the cave below.
        struct WaterCave;
        impl BlockSource for WaterCave {
            fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
                if y >= 0 {
                    return BlockStateId::AIR;
                }
                let shaft = x == 4 && z == 0 && y <= -1;
                let cave =
                    (3..=5).contains(&x) && (-1..=1).contains(&z) && (-40..=-38).contains(&y);
                if shaft || cave {
                    blocks::default_state("minecraft:water").unwrap()
                } else {
                    STONE
                }
            }
        }
        let _guard = global_test_lock();
        invalidate_all();
        let vis = visible_from_margin(&WaterCave, (0.5, 5.5, 0.5), 1, 2);
        let hidden = column_hidden(&WaterCave, &vis, 0, 0);
        assert!(
            hidden.contains(1, -20, 0),
            "the unseen water cave is hidden (block y=-40)"
        );
        assert!(
            hidden.contains(1, -10, 0),
            "the shaft deep below the surface is hidden (block y=-20)"
        );
        // The water touching the surface is visible; it just does not let the
        // search continue into the cave.
        assert!(
            !hidden.contains(1, -1, 0),
            "the water face at the surface is visible (block y=-1)"
        );
    }

    #[test]
    fn a_sub_cell_move_does_not_change_the_visible_set() {
        // Moving within a section must not reveal a hidden ore: the search is
        // seeded by section, so two positions in the same section answer the
        // same, and the reveal planner then has nothing to re-send.
        let _guard = global_test_lock();
        invalidate_all();
        let world = DeepGround {
            ore: ore("minecraft:diamond_ore"),
            ore_at: Some((40, -20, 0)),
        };
        let before = visible_from_margin(&world, (0.5, 5.5, 0.5), 2, 2);
        let after = visible_from_margin(&world, (1.4, 5.5, 0.5), 2, 2);
        assert_eq!(before, after, "a sub-cell move must not change the view");

        let hb = column_hidden(&world, &before, 2, 0);
        let ha = column_hidden(&world, &after, 2, 0);
        assert_eq!(hb, ha, "the masked column is identical");
        assert!(
            ha.contains(10, -10, 0),
            "the far ore cell stays hidden (block y=-20)"
        );
    }

    #[test]
    fn a_move_replans_only_columns_that_gained_a_cell() {
        // A column is re-sent only when one of its cells actually became
        // visible, never just because the search re-ran.
        let _guard = global_test_lock();
        invalidate_all();
        let world = DeepGround {
            ore: ore("minecraft:iron_ore"),
            ore_at: None,
        };
        let before = visible_from_margin(&world, (0.5, 5.5, 0.5), 2, 2);
        let after = visible_from_margin(&world, (20.5, 5.5, 0.5), 2, 2);

        let mut sent = HashMap::new();
        for cx in -4..=4 {
            for cz in -4..=4 {
                let hidden = column_hidden(&world, &before, cx, cz);
                if !hidden.is_empty() {
                    sent.insert((cx, cz), hidden);
                }
            }
        }
        let plan = plan_reveals(&world, &after, &sent, (1, 0, 0), 4096);
        for column in &plan {
            let desired = column_hidden(&world, &after, column.0, column.1);
            assert!(
                sent[column].has_extra(&desired),
                "a re-sent column must have gained a visible cell"
            );
        }
    }

    #[test]
    fn visibility_status_reports_the_cell_mode() {
        // `/visibility status` must name the live mode; the culling mode is
        // the cell culling now.
        let handle = PlayerHandle::for_test(1, "status-tester");
        handle
            .staff()
            .set_xray(crate::staff::AntiXrayLevel::Culling);
        let status = player_status(&handle);
        assert!(status.contains("culling"), "{status}");
        assert!(status.contains("4x4x2"), "{status}");
    }
}
