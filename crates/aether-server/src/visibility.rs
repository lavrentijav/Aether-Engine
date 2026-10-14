//! Visibility culling / anti-xray — the server half of the optional module
//! (`docs/design/visibility-culling.md`).
//!
//! The pure graph and the breadth-first search live in
//! [`aether_world::visibility`]; this module caches a section's graph across
//! players, keeps each player's reachable set up to date, and masks the
//! sections a player cannot see out of the column the codec is about to
//! encode. Off unless `[visibility] enabled = true`.
//!
//! The masking is a [`BlockSource`] wrapper: the codec reads a hidden
//! section and is handed `stone` (or `air`) for every one of its 4096 cells,
//! so the existing per-version encoders turn it into the single-valued stone
//! container 1.18+ already speaks, or a full stone section before that,
//! without any codec change. Nothing here edits a codec.
//!
//! The one deliberate simplification: the codecs that compute real light ask
//! the same [`BlockSource`] for blocks and for light, so a hidden section is
//! lit as the stone it is (block light 0, sky light 0). For the common hidden
//! section — solid rock — that is the real value anyway; a hidden section
//! holding a sealed, lit pocket would differ until it is revealed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use aether_world::visibility::{
    compute_section_graph, default_is_opaque, reachable_sections, SectionGraph,
};
use aether_world::BlockStateId;

use crate::config::HiddenFill;
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
}

impl State {
    /// Whether the cached reachable set already matches `section` and no
    /// in-view graph has changed since it was computed.
    pub fn is_fresh(&self, section: (i32, i32, i32)) -> bool {
        self.center == Some(section) && !self.dirty
    }

    /// Adopt a freshly computed reachable set.
    pub fn set_visible(&mut self, section: (i32, i32, i32), visible: HashSet<(i32, i32, i32)>) {
        self.center = Some(section);
        self.dirty = false;
        self.visible = visible;
        self.search_id = self.search_id.wrapping_add(1);
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

    /// Forget a column the client no longer has.
    pub fn forget(&mut self, cx: i32, cz: i32) {
        self.sent.remove(&(cx, cz));
    }

    /// Drop everything: a respawn starts the client with an empty level, so
    /// the columns it used to hold are no longer ours to reveal or mask.
    pub fn reset(&mut self) {
        self.center = None;
        self.dirty = true;
        self.visible.clear();
        self.sent.clear();
        self.search_id = self.search_id.wrapping_add(1);
        self.planned_search = self.search_id;
        self.reveal_pending = false;
    }

    /// Mark this player's reachable set stale when `key` falls inside the
    /// view box rooted at their current section.
    ///
    /// A change outside that box cannot alter the answer: the breadth-first
    /// search never looks outside it. The vertical axis is always in range,
    /// because the search walks the whole column.
    pub fn mark_dirty_if_in_view(&mut self, view_radius: i32, key: (i32, i32, i32)) {
        if let Some(c) = self.center {
            if (key.0 - c.0).abs() <= view_radius && (key.2 - c.2).abs() <= view_radius {
                self.dirty = true;
            }
        }
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

/// The shared section-graph cache, plus a per-section epoch.
///
/// The graph of a section depends only on the world, so one cache serves
/// every player. The epoch is what makes a concurrent write safe: a graph is
/// computed from a snapshot of the blocks, and it is only stored if the
/// section's epoch is unchanged between the snapshot and the store. A write
/// that lands in that window bumps the epoch and removes the entry, so the
/// stale graph is never cached (finding 4).
#[derive(Default)]
struct GraphCache {
    graphs: HashMap<(i32, i32, i32), SectionGraph>,
    epochs: HashMap<(i32, i32, i32), u64>,
}

/// Bump `key`'s epoch and drop any cached graph for it.
fn invalidate_section(cache: &mut GraphCache, key: (i32, i32, i32)) {
    *cache.epochs.entry(key).or_insert(0) += 1;
    cache.graphs.remove(&key);
}

/// Store `graph` for `key` only if no write touched `key` since the snapshot
/// `computed_at` was taken.
fn store_graph(
    cache: &mut GraphCache,
    key: (i32, i32, i32),
    graph: SectionGraph,
    computed_at: u64,
) {
    if cache.epochs.get(&key).copied().unwrap_or(0) == computed_at {
        cache.graphs.insert(key, graph);
    }
}

static CACHE: OnceLock<Mutex<GraphCache>> = OnceLock::new();

fn cache() -> &'static Mutex<GraphCache> {
    CACHE.get_or_init(|| Mutex::new(GraphCache::default()))
}

/// Forget a section's cached graph after a block in it changed, and mark the
/// players whose view covers it so their next search recomputes.
///
/// Called from every path that writes a block, so a broken wall reopens the
/// sight line to whatever was behind it. Only players whose view actually
/// contains the section are marked: a break on the far side of the map does
/// not force anyone else's search to rerun.
pub fn invalidate_block(x: i32, y: i32, z: i32) {
    let key = (x >> 4, y >> 4, z >> 4);
    invalidate_section(&mut cache().lock().unwrap(), key);
    if let Some(reg) = crate::game::registry() {
        let r = crate::server_view_radius();
        for player in reg.snapshot() {
            let mut st = player.visibility();
            if !st.bypass {
                st.mark_dirty_if_in_view(r, key);
            }
        }
    }
}

/// Drop every cached graph. Used by tests to start from a clean cache.
#[cfg(test)]
pub fn invalidate_all() {
    cache().lock().unwrap().graphs.clear();
}

/// The section graph for `c`, computed and cached on first ask.
///
/// `None` outside the world's vertical range, which stops the search at the
/// floor and ceiling.
fn graph(world: &dyn BlockSource, c: (i32, i32, i32)) -> Option<SectionGraph> {
    if !(SECTION_MIN_Y..=SECTION_MAX_Y).contains(&c.1) {
        return None;
    }
    let computed_at = {
        let cc = cache().lock().unwrap();
        if let Some(g) = cc.graphs.get(&c) {
            return Some(*g);
        }
        cc.epochs.get(&c).copied().unwrap_or(0)
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
    let margin = crate::visibility_config().margin_blocks as f64;
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
    let visible = visible_from(world, (pos.x, pos.y, pos.z), crate::server_view_radius());
    handle.visibility().set_visible(section, visible);
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
    match crate::visibility_config().hidden_fill {
        HiddenFill::Stone => STONE,
        HiddenFill::Air if mode == GameMode::Survival => STONE,
        HiddenFill::Air => BlockStateId::AIR,
    }
}

/// The hidden-section mask to encode column `(cx, cz)` with, recording it as
/// what the client now holds. `0` when culling is off or bypassed.
pub fn hidden_mask_for_render(
    handle: &PlayerHandle,
    world: &dyn BlockSource,
    cx: i32,
    cz: i32,
) -> u32 {
    if !crate::visibility_config().enabled || handle.visibility().bypass {
        return 0;
    }
    ensure_visible(handle, world);
    let mut st = handle.visibility();
    let mask = st.hidden_for(cx, cz);
    st.record_sent(cx, cz, mask);
    mask
}

/// Forget a column the client no longer has.
pub fn forget_column(handle: &PlayerHandle, cx: i32, cz: i32) {
    handle.visibility().forget(cx, cz);
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

/// One per-player reveal pass, run from the game tick.
///
/// A search only reruns when the player crosses a section border or an
/// in-view graph changed. A reveal that hit the budget keeps the plan pending
/// so the rest of the backlog goes out on following ticks.
pub fn tick(players: &[Arc<PlayerHandle>], world: &dyn BlockSource) {
    let cfg = crate::visibility_config();
    if !cfg.enabled {
        return;
    }
    for player in players {
        if player.visibility().bypass {
            continue;
        }
        ensure_visible(player, world);
        let plan = player.visibility().reveal_step(cfg.reveal_budget_per_tick);
        for (cx, cz) in plan {
            player.emit(&ServerEvent::ChunkColumn { cx, cz }, world);
        }
    }
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
    fn a_write_dirties_only_players_whose_view_covers_it() {
        // A break inside a player's view marks them; one on the far side of
        // the map does not (finding 5).
        let mut near = State {
            center: Some((0, 0, 0)),
            ..Default::default()
        };
        near.mark_dirty_if_in_view(8, (3, 100, 3));
        assert!(near.dirty, "a change in the view box must mark the player");
        assert!(!near.is_fresh((0, 0, 0)));

        let mut far = State {
            center: Some((0, 0, 0)),
            ..Default::default()
        };
        far.mark_dirty_if_in_view(8, (20, 0, 0));
        assert!(!far.dirty, "a change beyond the view box must not");
    }

    #[test]
    fn a_graph_snapshotted_before_a_write_is_never_cached() {
        // The in-flight-compute race (finding 4): a write lands after the
        // snapshot but before the store; the epoch guard must drop the stale
        // graph instead of caching it.
        let mut cache = GraphCache::default();
        let key = (0, 0, 0);
        let computed_at = cache.epochs.get(&key).copied().unwrap_or(0);
        invalidate_section(&mut cache, key);
        store_graph(&mut cache, key, SectionGraph::open(), computed_at);
        assert!(
            !cache.graphs.contains_key(&key),
            "a graph from a stale snapshot must not be cached"
        );

        // With no intervening write it is cached, and a later write removes it.
        let computed_at = cache.epochs.get(&key).copied().unwrap_or(0);
        store_graph(&mut cache, key, SectionGraph::open(), computed_at);
        assert!(cache.graphs.contains_key(&key));
        invalidate_section(&mut cache, key);
        assert!(!cache.graphs.contains_key(&key));
    }

    #[test]
    fn invalidating_a_section_drops_its_cached_graph() {
        // What `/rollback` now does for every restored block (finding 3): the
        // graph cache must forget the section so the reopened or resealed cave
        // is re-evaluated, not served from the old wall.
        invalidate_all();
        let world = FlatWorld;
        let key = (0, 2, 0);
        assert_eq!(
            graph(&world, key).unwrap().open_faces(),
            aether_world::visibility::ALL_FACES
        );
        assert!(cache().lock().unwrap().graphs.contains_key(&key));
        // invalidate_block takes block coordinates: (0, 32, 0) is section (0,2,0).
        invalidate_block(0, 32, 0);
        assert!(!cache().lock().unwrap().graphs.contains_key(&key));
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
        invalidate_all();
        let world = FlatWorld;
        let g = graph(&world, (0, -2, 0)).unwrap();
        assert_eq!(g.open_faces(), 0);
        let g = graph(&world, (0, 2, 0)).unwrap();
        assert_eq!(g.open_faces(), aether_world::visibility::ALL_FACES);
    }

    #[test]
    fn visible_from_hides_a_sealed_underground_column() {
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
}
