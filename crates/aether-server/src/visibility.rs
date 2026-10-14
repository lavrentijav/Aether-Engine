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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use aether_world::visibility::{
    compute_section_graph, default_is_opaque, reachable_sections, SectionGraph,
};
use aether_world::BlockStateId;

use crate::config::HiddenFill;
use crate::players::PlayerHandle;
use crate::protocol::{BlockSource, ServerEvent};

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
    pub center: Option<(i32, i32, i32)>,
    /// The graph generation the last search saw.
    pub generation: u64,
    /// Sections the player can currently see.
    pub visible: HashSet<(i32, i32, i32)>,
    /// For each column sent, the bitmask of hidden sections (bit `i` is
    /// section `SECTION_MIN_Y + i`). What the client currently holds.
    pub sent: HashMap<(i32, i32), u32>,
    /// The centre/generation a reveal plan was last computed for, so the
    /// bookkeeping runs only when something actually changed.
    planned_center: Option<(i32, i32, i32)>,
    planned_generation: u64,
}

/// The shared section-graph cache and a generation stamp.
///
/// The graph of a section depends only on the world, so one cache serves
/// every player. Whenever a block changes the affected section is dropped and
/// the stamp advanced, which every player notices on its next reveal pass.
static CACHE: OnceLock<Mutex<HashMap<(i32, i32, i32), SectionGraph>>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn cache() -> &'static Mutex<HashMap<(i32, i32, i32), SectionGraph>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Forget a section's cached graph after a block in it changed.
///
/// Called from every path that writes a block, so a broken wall reopens the
/// sight line to whatever was behind it. The generation is only advanced when
/// a graph was actually cached — a change in an untouched section cannot
/// change anyone's answer.
pub fn invalidate_block(x: i32, y: i32, z: i32) {
    let key = (x >> 4, y >> 4, z >> 4);
    if cache().lock().unwrap().remove(&key).is_some() {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Drop every cached graph. Used by tests to start from a clean cache.
#[cfg(test)]
pub fn invalidate_all() {
    cache().lock().unwrap().clear();
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The section graph for `c`, computed and cached on first ask.
///
/// `None` outside the world's vertical range, which stops the search at the
/// floor and ceiling.
fn graph(world: &dyn BlockSource, c: (i32, i32, i32)) -> Option<SectionGraph> {
    if !(SECTION_MIN_Y..=SECTION_MAX_Y).contains(&c.1) {
        return None;
    }
    if let Some(g) = cache().lock().unwrap().get(&c) {
        return Some(*g);
    }
    let mut blocks = [BlockStateId::AIR; 4096];
    world.copy_section(c.0, c.1, c.2, &mut blocks);
    let g = compute_section_graph(&blocks, default_is_opaque);
    cache().lock().unwrap().insert(c, g);
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

/// Recompute a player's reachable set if the player moved to a new section or
/// a graph changed since the last search.
fn ensure_visible(handle: &PlayerHandle, world: &dyn BlockSource) {
    let pos = handle.pos();
    let section = section_of(pos);
    let generation = GENERATION.load(Ordering::Relaxed);
    {
        let st = handle.visibility();
        if st.center == Some(section) && st.generation == generation {
            return;
        }
    }
    let cfg = crate::visibility_config();
    let visible = reachable_sections(
        (pos.x, pos.y, pos.z),
        cfg.margin_blocks as f64,
        crate::server_view_radius(),
        SECTION_MIN_Y,
        SECTION_MAX_Y,
        |x, y, z| graph(world, (x, y, z)),
    );
    let mut st = handle.visibility();
    st.center = Some(section);
    st.generation = generation;
    st.visible = visible;
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
    match crate::visibility_config().hidden_fill {
        HiddenFill::Stone => STONE,
        HiddenFill::Air if handle.game().survival() => STONE,
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
    let mask = desired_hidden(&st.visible, cx, cz);
    st.sent.insert((cx, cz), mask);
    mask
}

/// Forget a column the client no longer has.
pub fn forget_column(handle: &PlayerHandle, cx: i32, cz: i32) {
    handle.visibility().sent.remove(&(cx, cz));
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
/// The search itself only reruns on a section-border crossing or a graph
/// change; the reveal plan is only recomputed when either happened, so an
/// idle player costs a lock and nothing else.
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
        let (center, generation) = {
            let st = player.visibility();
            (st.center, st.generation)
        };
        let plan = {
            let mut st = player.visibility();
            if st.planned_center == center && st.planned_generation == generation {
                continue;
            }
            st.planned_center = center;
            st.planned_generation = generation;
            plan_reveals(
                &st.visible,
                &st.sent,
                center.unwrap_or((0, 0, 0)),
                cfg.reveal_budget_per_tick,
            )
        };
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
    fn the_reveal_plan_is_nearest_first_and_budgeted() {
        // Five columns were sent fully hidden; all become visible. Only the
        // two nearest fit the budget.
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
        assert_eq!(plan, vec![(0, 0), (1, 0)], "nearest first");
    }

    #[test]
    fn no_reveal_when_nothing_became_visible() {
        // Every column already holds the right mask: nothing to re-send.
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
    fn masked_column_reports_the_fill_in_a_hidden_section_only() {
        // Hide section y=0 (index 4): the real air above the surface must
        // come back as stone, the solid section below is stone either way,
        // and a column we did not mask is untouched.
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
        // y=-2 is entirely below the surface: solid stone, nothing connects.
        let g = graph(&world, (0, -2, 0)).unwrap();
        assert_eq!(g.open_faces(), 0);
        // y=2 is all air: every face open and connected.
        let g = graph(&world, (0, 2, 0)).unwrap();
        assert_eq!(g.open_faces(), aether_world::visibility::ALL_FACES);
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
