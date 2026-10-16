//! Anti-xray — the server half of the optional module
//! (`docs/design/visibility-culling.md`).
//!
//! Two masking strategies, selected by `[anti_xray] mode`:
//! - **engine** (cheap, default): an ore-like block that is fully enclosed by
//!   opaque blocks is sent as the surrounding stone/deepslate/netherrack, and
//!   revealed again the moment a neighbour changes so it is on the client
//!   before the player can mine to it.
//! - **culling** (the design doc): the pure graph and breadth-first search in
//!   [`aether_world::visibility`] decide which sections a player can reach by
//!   sight; an unreachable section is sent as solid stone.
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
    compute_section_graph, default_is_opaque, reachable_sections, SectionGraph,
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

/// Solid stone, what a hidden section is filled with by default.
const STONE: BlockStateId = aether_world::registry::ids::STONE;

/// One player's visibility bookkeeping.
///
/// `Default` is the state before the first search: no centre, nothing known.
#[derive(Default)]
pub struct State {
    /// Set by `/visibility off`: this player sees everything.
    pub bypass: bool,
    /// The section the last search was rooted at.
    center: Option<(i32, i32, i32)>,
    /// A graph inside this player's view changed since the last search. Set
    /// from a writer thread, cleared by the next search.
    dirty: bool,
    /// Bumped on every search. A reveal plan computed for one search id is not
    /// recomputed for the same id unless it was cut short by the budget.
    search_id: u64,
    /// Sections the player can currently see.
    visible: HashSet<(i32, i32, i32)>,
    /// For each column sent, the bitmask of hidden sections (bit `i` is
    /// section `SECTION_MIN_Y + i`). What the client currently holds.
    sent: HashMap<(i32, i32), u32>,
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

    /// Force the next search to recompute, even at the same section. Used when
    /// a `/spectate xray` margin change makes the old reachable set wrong.
    pub fn invalidate(&mut self) {
        self.center = None;
    }

    /// Adopt a freshly computed reachable set.
    ///
    /// `epoch_at_start` is the global edit epoch when the search began. The
    /// stale flag is only cleared if no block was written while the search
    /// ran: a write that landed mid-search is not lost, the next search runs
    /// again.
    pub fn set_visible_if(
        &mut self,
        section: (i32, i32, i32),
        visible: HashSet<(i32, i32, i32)>,
        epoch_at_start: u64,
    ) {
        self.center = Some(section);
        self.visible = visible;
        self.search_id = self.search_id.wrapping_add(1);
        if EDIT_EPOCH.load(Ordering::Relaxed) == epoch_at_start {
            self.dirty = false;
        }
    }

    /// Adopt a reachable set together with the columns already sent, for the
    /// join path where the first columns go out before a handle exists.
    pub fn seed(
        &mut self,
        section: (i32, i32, i32),
        visible: HashSet<(i32, i32, i32)>,
        sent: HashMap<(i32, i32), u32>,
    ) {
        self.center = Some(section);
        self.dirty = false;
        self.visible = visible;
        self.sent = sent;
        self.search_id = self.search_id.wrapping_add(1);
        self.planned_search = self.search_id;
        self.reveal_pending = false;
    }

    /// The hidden-section mask for a column of the current reachable set.
    pub fn hidden_for(&self, cx: i32, cz: i32) -> u32 {
        desired_hidden(&self.visible, cx, cz)
    }

    /// Record the mask a column was actually sent with.
    pub fn record_sent(&mut self, cx: i32, cz: i32, mask: u32) {
        self.sent.insert((cx, cz), mask);
    }

    /// The mask to render column `(cx, cz)` with, updating what the client
    /// holds.
    ///
    /// `bypass` is the caller's current flag. It matters even on the last
    /// column: `/visibility off` re-renders the columns it had masked, and
    /// recording the zero mask here is what tells the planner the client now
    /// holds the real blocks (round-2 N4 / round-3 New-2).
    pub fn mask_for_render(&mut self, cx: i32, cz: i32, bypass: bool) -> u32 {
        let mask = if bypass { 0 } else { self.hidden_for(cx, cz) };
        self.record_sent(cx, cz, mask);
        mask
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
        self.visible.clear();
        self.sent.clear();
        self.revealed.clear();
        self.search_id = self.search_id.wrapping_add(1);
        self.planned_search = self.search_id;
        self.reveal_pending = false;
    }

    /// Mark this player's reachable set stale if any of `sections` falls
    /// inside the view box rooted at their current section.
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
    pub fn reveal_step(&mut self, budget: u32) -> Vec<(i32, i32)> {
        if budget == 0 {
            return Vec::new();
        }
        if self.planned_search == self.search_id && !self.reveal_pending {
            return Vec::new();
        }
        self.planned_search = self.search_id;
        let plan = plan_reveals(
            &self.visible,
            &self.sent,
            self.center.unwrap_or((0, 0, 0)),
            budget,
        );
        self.reveal_pending = plan.len() as u32 == budget;
        plan
    }

    /// The sections this player can currently see (tests and diagnostics).
    pub fn visible(&self) -> &HashSet<(i32, i32, i32)> {
        &self.visible
    }

    /// The columns sent and their last-known hidden masks (tests).
    pub fn sent(&self) -> &HashMap<(i32, i32), u32> {
        &self.sent
    }
}

/// The shared section-graph cache.
///
/// The graph of a section depends only on the world, so one cache serves every
/// player. A graph is computed from a snapshot of the blocks *outside* the
/// lock; it is stored only if the global edit epoch is unchanged between the
/// snapshot and the store, so a write that lands in that window discards the
/// stale graph (finding 4).
///
/// The epoch is global and monotonic rather than a per-section counter that is
/// reset on store: a per-section reset lets an in-flight compute that read the
/// reset value be re-admitted after a newer graph is stored, poisoning the
/// cache with a pre-write graph (round-3 New-1). A global version that only
/// ever increases cannot be re-created, and it removes the per-section map
/// entirely, so there is nothing to grow without bound (round-2 N3).
#[derive(Default)]
struct GraphCache {
    graphs: HashMap<(i32, i32, i32), SectionGraph>,
}

/// Store `graph` for `key` only if no write landed since `computed_at` was
/// read.
fn store_graph(
    cache: &mut GraphCache,
    key: (i32, i32, i32),
    graph: SectionGraph,
    computed_at: u64,
) {
    if EDIT_EPOCH.load(Ordering::Relaxed) == computed_at {
        cache.graphs.insert(key, graph);
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

/// Drop `key`'s cached graph and advance the epoch.
fn invalidate_section(cache: &mut GraphCache, key: (i32, i32, i32)) {
    cache.graphs.remove(&key);
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
    match crate::anti_xray_config().mode {
        AntiXrayMode::Culling => record_invalidation((x >> 4, y >> 4, z >> 4)),
        AntiXrayMode::Engine => {
            dirty_blocks().lock().unwrap().insert((x, y, z));
        }
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
    cache().lock().unwrap().graphs.clear();
    dirty_sections().lock().unwrap().clear();
    EDIT_EPOCH.fetch_add(1, Ordering::Relaxed);
}

/// The section graph for `c`, computed and cached on first ask.
///
/// `None` outside the world's vertical range, which stops the search at the
/// floor and ceiling.
fn graph(world: &dyn BlockSource, c: (i32, i32, i32)) -> Option<SectionGraph> {
    if !(SECTION_MIN_Y..=SECTION_MAX_Y).contains(&c.1) {
        return None;
    }
    // Read the epoch under the lock, at the same moment the miss is observed,
    // so the version seen is exactly the one that was current when we decided
    // to compute: an invalidation that lands after this point bumps the epoch
    // and is caught at the store (finding 4).
    let computed_at = {
        let cc = cache().lock().unwrap();
        if let Some(g) = cc.graphs.get(&c) {
            return Some(*g);
        }
        EDIT_EPOCH.load(Ordering::Relaxed)
    };
    let mut blocks = [BlockStateId::AIR; 4096];
    world.copy_section(c.0, c.1, c.2, &mut blocks);
    let g = compute_section_graph(&blocks, default_is_opaque);
    store_graph(&mut cache().lock().unwrap(), c, g, computed_at);
    Some(g)
}

/// The section a player's feet are in.
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

/// The reachable section set with an explicit margin, for the per-player
/// anti-xray override (`/spectate xray <level>`).
pub fn visible_from_margin(
    world: &dyn BlockSource,
    pos: (f64, f64, f64),
    view_distance: i32,
    margin_blocks: i32,
) -> HashSet<(i32, i32, i32)> {
    let margin = margin_blocks as f64;
    reachable_sections(
        pos,
        margin,
        view_distance,
        SECTION_MIN_Y,
        SECTION_MAX_Y,
        |x, y, z| graph(world, (x, y, z)),
    )
}

/// Recompute a player's reachable set if the player moved to a new section or
/// an in-view graph changed since the last search.
fn ensure_visible(handle: &PlayerHandle, world: &dyn BlockSource) {
    let pos = handle.pos();
    let section = section_of(pos);
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
            "anti-xray: culling, margin {} blocks, {hidden} hidden block(s)",
            effective_margin(handle)
        ),
    }
}

/// Which sections of column `(cx, cz)` are hidden for this visible set.
///
/// Bit `i` set means section `SECTION_MIN_Y + i` is not visible.
pub fn desired_hidden(visible: &HashSet<(i32, i32, i32)>, cx: i32, cz: i32) -> u32 {
    let mut mask = 0u32;
    for i in 0..SECTION_COUNT {
        let cy = SECTION_MIN_Y + i as i32;
        if !visible.contains(&(cx, cy, cz)) {
            mask |= 1 << i;
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
    /// `culling`: encode these sections (bit `i` is `SECTION_MIN_Y + i`) as
    /// `fill`.
    Sections { hidden: u32, fill: BlockStateId },
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
            let hidden = handle.visibility().mask_for_render(cx, cz, bypass);
            if hidden == 0 {
                ColumnMask::None
            } else {
                ColumnMask::Sections {
                    hidden,
                    fill: fill_for(handle),
                }
            }
        }
        // An engine column is always built (it only pays for the hidden-block
        // lookup); a bypassed player gets the plain view and records nothing.
        AntiXrayMode::Engine => {
            if bypass {
                handle.visibility().record_sent(cx, cz, 0);
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
    handle.visibility().record_sent(cx, cz, masked as u32);
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
            if section_hidden(handle, world, x >> 4, y >> 4, z >> 4) {
                fill_for(handle)
            } else {
                block
            }
        }
    }
}

/// Whether section `(cx, cy, cz)` is hidden from this player in culling mode.
fn section_hidden(
    handle: &PlayerHandle,
    world: &dyn BlockSource,
    cx: i32,
    cy: i32,
    cz: i32,
) -> bool {
    ensure_visible(handle, world);
    !handle.visibility().visible().contains(&(cx, cy, cz))
}

/// Forget a column the client no longer has.
pub fn forget_column(handle: &PlayerHandle, cx: i32, cz: i32) {
    handle.visibility().forget(cx, cz);
}

/// The sections whose mask is currently non-zero, for `/visibility off`.
fn masked_columns(sent: &HashMap<(i32, i32), u32>) -> Vec<(i32, i32)> {
    sent.iter()
        .filter(|(_, mask)| **mask != 0)
        .map(|(&column, _)| column)
        .collect()
}

/// Re-send every column this client currently holds masked, so `/visibility
/// off` shows the real blocks without waiting for columns to re-stream.
///
/// The caller sets `bypass` first: with it on, re-encoding a column records a
/// zero mask, so the client ends up with every section real and the reveal
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
/// A reveal is a section whose bit is set in what the client holds but clear
/// in the current visible set. Sections that went the other way (visible,
/// then hidden) are left as they are: the client has already seen them, and
/// re-hiding would cost a full column re-send for no gain.
pub fn plan_reveals(
    visible: &HashSet<(i32, i32, i32)>,
    sent: &HashMap<(i32, i32), u32>,
    player_section: (i32, i32, i32),
    budget: u32,
) -> Vec<(i32, i32)> {
    let (px, pz) = (player_section.0, player_section.2);
    let mut candidates: Vec<((i32, i32), i64)> = Vec::new();
    for (&(cx, cz), &old) in sent {
        let now = desired_hidden(visible, cx, cz);
        if old & !now != 0 {
            let dx = (cx - px) as i64;
            let dz = (cz - pz) as i64;
            candidates.push(((cx, cz), dx * dx + dz * dz));
        }
    }
    candidates.sort_by_key(|&(_, distance)| distance);
    candidates.truncate(budget as usize);
    candidates.into_iter().map(|(column, _)| column).collect()
}

/// One per-player pass, run from the game tick.
///
/// `culling`: a search only reruns when the player crosses a section border or
/// an in-view graph changed; a reveal that hit the budget keeps the plan
/// pending so the rest of the backlog goes out on following ticks.
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
        let plan = player.visibility().reveal_step(cfg.reveal_budget_per_tick);
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

/// The startup log line for the effective anti-xray configuration:
/// `anti-xray  : off`, or
/// `anti-xray  : <mode> (<worlds>, hidden N blocks)`.
///
/// Reads the globals, so it must be called after startup installs the config
/// (where the log line is emitted). `off` is the *effective* answer for the
/// running world — the module is disabled or `[anti_xray] worlds` does not
/// name this world directory — so the line never claims it is on when
/// [`crate::anti_xray_enabled`] says no.
pub fn status_line() -> String {
    status_for(crate::anti_xray_enabled(), &crate::anti_xray_config())
}

/// The pure formatter behind [`status_line`], so the wording is testable
/// without the process-wide config and the cached-enabled flag.
fn status_for(enabled: bool, cfg: &crate::config::AntiXrayConfig) -> String {
    if !enabled {
        return "anti-xray  : off".to_string();
    }
    let mode = match cfg.mode {
        AntiXrayMode::Engine => "engine",
        AntiXrayMode::Culling => "culling",
    };
    let worlds = if cfg.worlds.is_empty() {
        "all worlds".to_string()
    } else {
        format!("worlds {}", cfg.worlds.join(", "))
    };
    let hidden = cfg.hidden_block_names().len();
    format!("anti-xray  : {mode} ({worlds}, hidden {hidden} blocks)")
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

/// A [`BlockSource`] that reports a hidden section as a single repeated block.
///
/// The wrapper is what makes the anti-xray encoding codec-agnostic: every
/// version's chunk writer already turns 4096 identical blocks into its own
/// cheapest section form, so nothing downstream has to know about visibility.
pub struct MaskedColumn<'a> {
    inner: &'a dyn BlockSource,
    cx: i32,
    cz: i32,
    hidden: u32,
    fill: BlockStateId,
}

impl<'a> MaskedColumn<'a> {
    /// Mask `hidden` (bit `i` is section `SECTION_MIN_Y + i`) of column
    /// `(cx, cz)` with `fill`.
    pub fn new(
        inner: &'a dyn BlockSource,
        cx: i32,
        cz: i32,
        hidden: u32,
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

    fn hides(&self, cx: i32, cy: i32, cz: i32) -> bool {
        if cx != self.cx || cz != self.cz {
            return false;
        }
        if !(SECTION_MIN_Y..=SECTION_MAX_Y).contains(&cy) {
            return false;
        }
        let i = (cy - SECTION_MIN_Y) as u32;
        self.hidden & (1 << i) != 0
    }
}

impl BlockSource for MaskedColumn<'_> {
    fn block_at(&self, x: i32, y: i32, z: i32) -> BlockStateId {
        if self.hides(x >> 4, y >> 4, z >> 4) {
            return self.fill;
        }
        self.inner.block_at(x, y, z)
    }

    fn copy_section(&self, cx: i32, cy: i32, cz: i32, out: &mut [BlockStateId; 4096]) {
        if self.hides(cx, cy, cz) {
            out.fill(self.fill);
            return;
        }
        self.inner.copy_section(cx, cy, cz, out);
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

    fn visible(sections: &[(i32, i32, i32)]) -> HashSet<(i32, i32, i32)> {
        sections.iter().copied().collect()
    }

    /// A state whose reachable set is every section of `columns`.
    fn state_showing(columns: &[(i32, i32)], search_id: u64) -> State {
        let mut vis = HashSet::new();
        for &(cx, cz) in columns {
            for i in 0..SECTION_COUNT {
                vis.insert((cx, SECTION_MIN_Y + i as i32, cz));
            }
        }
        State {
            center: Some((0, 0, 0)),
            dirty: false,
            search_id,
            visible: vis,
            sent: HashMap::new(),
            planned_search: search_id,
            reveal_pending: false,
            bypass: false,
            revealed: HashSet::new(),
        }
    }

    #[test]
    fn a_fully_visible_column_has_no_hidden_sections() {
        let all: Vec<(i32, i32, i32)> = (0..SECTION_COUNT)
            .map(|i| (0, SECTION_MIN_Y + i as i32, 0))
            .collect();
        assert_eq!(desired_hidden(&visible(&all), 0, 0), 0);
    }

    #[test]
    fn a_sealed_cave_section_is_hidden() {
        // Everything visible except the sections at y=-2 and y=-1 (a cave
        // behind rock), which must stay hidden. Index `i` is `cy =
        // SECTION_MIN_Y + i`, so y=-2 is 2 and y=-1 is 3.
        let all: Vec<(i32, i32, i32)> = (0..SECTION_COUNT)
            .filter_map(|i| {
                let cy = SECTION_MIN_Y + i as i32;
                (!(-2..=-1).contains(&cy)).then_some((0, cy, 0))
            })
            .collect();
        let mask = desired_hidden(&visible(&all), 0, 0);
        assert_eq!(mask, (1 << 2) | (1 << 3));
    }

    #[test]
    fn a_reveal_plan_cut_short_by_the_budget_continues_next_tick() {
        // Every column is sent fully hidden and every section is now visible:
        // six columns, two per tick. All six must go out across ticks, not
        // just the first two (finding 1).
        let columns = [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0)];
        let mut st = state_showing(&columns, 1);
        // The masks are stale relative to the fresh reachable set, so a plan
        // is due (the planner has not run for this search id yet).
        st.planned_search = 0;
        for &col in &columns {
            st.record_sent(col.0, col.1, u32::MAX);
        }
        let mut revealed = Vec::new();
        let mut ticks = 0;
        loop {
            let plan = st.reveal_step(2);
            if plan.is_empty() {
                break;
            }
            ticks += 1;
            assert!(ticks < 100, "the reveal must make progress");
            for (cx, cz) in plan {
                let mask = st.hidden_for(cx, cz);
                st.record_sent(cx, cz, mask);
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
        let all: Vec<(i32, i32, i32)> = (0..SECTION_COUNT)
            .map(|i| (0, SECTION_MIN_Y + i as i32, 0))
            .collect();
        let vis = visible(&all);
        let mut sent = HashMap::new();
        for (cx, cz) in [(0, 0), (1, 0), (5, 0), (2, 0), (10, 0)] {
            sent.insert((cx, cz), u32::MAX);
        }
        let plan = plan_reveals(&vis, &sent, (0, 0, 0), 2);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], (0, 0));
        assert!(plan.contains(&(1, 0)), "nearest first: {plan:?}");
    }

    #[test]
    fn no_reveal_when_nothing_became_visible() {
        let all: Vec<(i32, i32, i32)> = (0..SECTION_COUNT)
            .map(|i| (0, SECTION_MIN_Y + i as i32, 0))
            .collect();
        let vis = visible(&all);
        let mut sent = HashMap::new();
        sent.insert((0, 0), 0);
        sent.insert((1, 0), 0);
        assert!(plan_reveals(&vis, &sent, (0, 0, 0), 32).is_empty());
    }

    #[test]
    fn an_idle_player_does_not_replan() {
        let columns = [(0, 0), (1, 0)];
        let mut st = state_showing(&columns, 7);
        for &col in &columns {
            st.record_sent(col.0, col.1, 0);
        }
        assert!(st.reveal_step(32).is_empty());
        // Still nothing a second time.
        assert!(st.reveal_step(32).is_empty());
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
        st.record_sent(0, 0, u32::MAX);
        st.record_sent(1, 0, u32::MAX);
        st.reset();
        assert!(st.sent().is_empty(), "reset drops the old columns");
        assert!(st.visible().is_empty());
        assert!(st.reveal_step(32).is_empty());
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
        store_graph(&mut cc, key, SectionGraph::sealed(), a_version);
        assert!(cc.graphs.contains_key(&key), "A's fresh graph is cached");

        // B: stores its stale pre-write graph with the version it read.
        store_graph(&mut cc, key, SectionGraph::open(), b_version);
        assert_eq!(
            cc.graphs.get(&key).copied(),
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
        assert!(graph(&WriteDuringSnapshot, key).is_some());
        assert!(
            !cache().lock().unwrap().graphs.contains_key(&key),
            "a graph whose snapshot raced a write must not be cached"
        );
        // The same call without a racing write caches it, and a later write
        // evicts it.
        assert!(graph(&FlatWorld, key).is_some());
        assert!(
            cache().lock().unwrap().graphs.contains_key(&key),
            "an un-raced graph is cached"
        );
        record_invalidation(key);
        assert!(!cache().lock().unwrap().graphs.contains_key(&key));
    }

    /// Serialises the tests that touch the process-wide cache, epoch and
    /// dirty set, which are shared across the whole test binary.
    fn global_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn the_startup_line_names_the_mode_worlds_and_hidden_count() {
        use crate::config::{AntiXrayConfig, AntiXrayMode};
        // Disabled (or not applicable to this world): a plain "off", never a
        // mode that would overstate what the server does.
        let cfg = AntiXrayConfig::default();
        assert_eq!(status_for(false, &cfg), "anti-xray  : off");

        // Enabled: the mode, the worlds and the hidden-block count. The
        // default config's hidden list is the built-in ore set.
        let cfg = AntiXrayConfig {
            enabled: true,
            mode: AntiXrayMode::Engine,
            ..AntiXrayConfig::default()
        };
        let line = status_for(true, &cfg);
        assert_eq!(
            line,
            format!(
                "anti-xray  : engine (all worlds, hidden {} blocks)",
                crate::config::default_hidden_blocks().len()
            ),
            "got {line:?}"
        );

        // Culling with an explicit world list and an explicit hidden list.
        let cfg = AntiXrayConfig {
            enabled: true,
            mode: AntiXrayMode::Culling,
            worlds: vec!["alpha".to_string(), "beta".to_string()],
            hidden_blocks: vec!["minecraft:diamond_ore".to_string()],
            ..AntiXrayConfig::default()
        };
        assert_eq!(
            status_for(true, &cfg),
            "anti-xray  : culling (worlds alpha, beta, hidden 1 blocks)"
        );
    }

    #[test]
    fn write_invalidation_is_free_when_off_and_records_when_on() {
        // N1: with culling off (the default, and what every test here runs
        // under) a block write must not touch the cache or record a section.
        // Once on, it records the section and evicts the graph.
        let _guard = global_test_lock();
        invalidate_all();
        graph(&FlatWorld, (0, 2, 0));
        invalidate_block(0, 32, 0);
        assert!(
            dirty_sections().lock().unwrap().is_empty(),
            "a disabled write must not record a section"
        );
        assert!(
            cache().lock().unwrap().graphs.contains_key(&(0, 2, 0)),
            "a disabled write must not evict a cached graph"
        );

        // The enabled path (what `invalidate_block` runs once the flag is on).
        record_invalidation((0, 2, 0));
        assert!(!cache().lock().unwrap().graphs.contains_key(&(0, 2, 0)));
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
        sent.insert((0, 0), 0);
        sent.insert((1, 0), u32::MAX);
        sent.insert((2, 0), 1 << 3);
        let mut got = masked_columns(&sent);
        got.sort();
        assert_eq!(got, vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn rendering_a_bypassed_column_records_a_zero_mask() {
        // N4 / New-2: `/visibility off` re-renders the masked columns; each
        // must record a zero mask, or the planner keeps believing the client
        // holds stone and re-plans columns it already has.
        let columns = [(0, 0), (1, 0)];
        let mut st = state_showing(&columns, 1);
        for &col in &columns {
            st.record_sent(col.0, col.1, u32::MAX);
        }
        assert_eq!(masked_columns(st.sent()).len(), 2, "both start masked");

        // The reveal pass re-renders each column with bypass on.
        for (cx, cz) in masked_columns(st.sent()) {
            assert_eq!(st.mask_for_render(cx, cz, true), 0);
        }
        assert!(
            masked_columns(st.sent()).is_empty(),
            "after reveal the client holds real columns and `sent` says so"
        );

        // A non-bypassed render still records the real mask: a column the
        // player cannot see comes back with every section hidden.
        let all_hidden = (1u32 << SECTION_COUNT) - 1;
        assert_eq!(st.mask_for_render(9, 9, false), all_hidden);
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
    fn a_flat_world_below_the_surface_caches_a_sealed_graph() {
        let _guard = global_test_lock();
        invalidate_all();
        let world = FlatWorld;
        let g = graph(&world, (0, -2, 0)).unwrap();
        assert_eq!(g.open_faces(), 0);
        let g = graph(&world, (0, 2, 0)).unwrap();
        assert_eq!(g.open_faces(), aether_world::visibility::ALL_FACES);
    }

    #[test]
    fn visible_from_hides_a_sealed_underground_column() {
        let _guard = global_test_lock();
        invalidate_all();
        // The player is on the surface of the flat world: the solid sections
        // far below must not be in the reachable set.
        let vis = visible_from(&FlatWorld, (0.5, 5.5, 0.5), 8);
        assert!(
            vis.contains(&(0, 0, 0)),
            "the player's own section is visible"
        );
        assert!(!vis.contains(&(0, -4, 0)), "bedrock depth must stay hidden");
    }

    #[test]
    fn the_spawn_column_is_masked_the_way_the_join_path_masks_it() {
        // The join path masks its first columns through exactly these calls
        // (finding 2). A buried ore a section below the surface must not reach
        // the client as itself.
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
        let vis = visible_from(&world, (0.5, 5.5, 0.5), 8);
        let hidden = desired_hidden(&vis, 0, 0);
        assert_ne!(hidden, 0, "the deep sections must be hidden");
        let masked = MaskedColumn::new(&world, 0, 0, hidden, fill_for_mode(GameMode::Creative));
        assert_eq!(
            masked.block_at(0, -48, 0),
            STONE,
            "the buried ore must be sent as stone"
        );
        // A column the mask does not cover keeps the real block.
        let unmasked = MaskedColumn::new(&world, 1, 0, hidden, STONE);
        assert_eq!(unmasked.block_at(0, -48, 0), BlockStateId(1234));
    }

    #[test]
    fn section_of_floors_negative_coordinates() {
        let pos = crate::players::PosLook {
            x: -0.5,
            y: -1.0,
            z: 15.9,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        };
        assert_eq!(section_of(pos), (-1, -1, 0));
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
}
