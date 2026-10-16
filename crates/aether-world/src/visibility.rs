//! Section visibility graph and the per-player reachable set — the pure half
//! of the optional anti-xray / visibility-culling module
//! (`docs/design/visibility-culling.md`).
//!
//! Nothing here talks to a client or to the network. Given the blocks of one
//! 16³ section it computes which of the section's six faces see each other
//! through non-opaque space, and given a way to ask for a section's graph it
//! runs the breadth-first search a server uses to decide which sections a
//! player can see. Both are deterministic and side-effect free, so the
//! decisions can be tested without a socket.
//!
//! The graph is the same idea as the vanilla client's own `VisGraph`: a flood
//! fill over the non-opaque cells of a section, recording which boundary faces
//! each connected pocket touches. Two faces of the same pocket are connected,
//! which is exactly "you can enter here and see out there".

use std::collections::{HashMap, HashSet, VecDeque};

use crate::block::BlockStateId;

/// Blocks along one axis of a section.
pub const SECTION_SIZE: i32 = 16;
/// Cells in a section (`16³`), indexed `x | z << 4 | y << 8`.
pub const SECTION_VOLUME: usize = (SECTION_SIZE * SECTION_SIZE * SECTION_SIZE) as usize;

/// The face of a section, numbered so that opposite faces are `face ^ 1`.
pub const FACE_NEG_X: u8 = 0;
/// The `+X` face.
pub const FACE_POS_X: u8 = 1;
/// The `-Y` face.
pub const FACE_NEG_Y: u8 = 2;
/// The `+Y` face.
pub const FACE_POS_Y: u8 = 3;
/// The `-Z` face.
pub const FACE_NEG_Z: u8 = 4;
/// The `+Z` face.
pub const FACE_POS_Z: u8 = 5;
/// All six faces.
pub const ALL_FACES: u8 = 0x3F;

/// The face on the far side of a section from `face`.
#[inline]
pub const fn opposite(face: u8) -> u8 {
    face ^ 1
}

/// The section-coordinate step to the neighbour across `face`.
#[inline]
pub const fn face_offset(face: u8) -> (i32, i32, i32) {
    match face {
        FACE_NEG_X => (-1, 0, 0),
        FACE_POS_X => (1, 0, 0),
        FACE_NEG_Y => (0, -1, 0),
        FACE_POS_Y => (0, 1, 0),
        FACE_NEG_Z => (0, 0, -1),
        _ => (0, 0, 1),
    }
}

/// Which faces of one section see each other through passable blocks.
///
/// Two notions, deliberately separate:
/// - `open` is the set of faces that have at least one **non-opaque** cell on
///   them — you can see that face (stone/air/water/glass). It decides whether a
///   neighbouring section is *visible* at all.
/// - `air` is the set of faces with a **passable** (air-like) cell, and
///   `adj[f]` is the set of faces reachable from `f` through passable cells.
///   It decides whether the search may *continue* into a neighbour. A fluid or
///   glass window is visible but does not let the search through, so a pool the
///   player looks at cannot reveal the flooded caves it connects to.
///
/// `adj[f]` always contains `f` when `f` is an air face.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct SectionGraph {
    open: u8,
    air: u8,
    adj: [u8; 6],
    full_clear: bool,
}

impl SectionGraph {
    /// A fully opaque section: no face is a way in, nothing connects.
    pub const fn sealed() -> Self {
        Self {
            open: 0,
            air: 0,
            adj: [0; 6],
            full_clear: false,
        }
    }

    /// A fully open section: every face is a way in and every face sees every
    /// other — the answer for pure air.
    pub const fn open() -> Self {
        Self {
            open: ALL_FACES,
            air: ALL_FACES,
            adj: [ALL_FACES; 6],
            full_clear: true,
        }
    }

    /// The faces with at least one non-opaque cell — the faces you can see.
    #[inline]
    pub fn open_faces(&self) -> u8 {
        self.open
    }

    /// Whether the whole section has no opaque cell at all — an all-air or
    /// fully-flooded section, which the visibility wave may pass through
    /// without the air search (so an ocean stays water, but its stone floor
    /// and any cave walls stop it).
    #[inline]
    pub fn is_full_clear(&self) -> bool {
        self.full_clear
    }

    /// Whether `face` has any non-opaque cell.
    #[inline]
    pub fn is_open(&self, face: u8) -> bool {
        self.open & (1 << face) != 0
    }

    /// The faces with at least one passable (air-like) cell.
    #[inline]
    pub fn air_faces(&self) -> u8 {
        self.air
    }

    /// Whether `face` has any passable (air-like) cell, so the search may
    /// enter the section through it.
    #[inline]
    pub fn is_air(&self, face: u8) -> bool {
        self.air & (1 << face) != 0
    }

    /// Whether standing at `from` one can see the opening at `to`.
    #[inline]
    pub fn connects(&self, from: u8, to: u8) -> bool {
        self.adj[(from & 7) as usize] & (1 << to) != 0
    }

    /// The set of faces reachable starting from the openings in `entry`.
    ///
    /// This is the transitive closure of [`Self::connects`] from every open
    /// face in `entry`; a fixed point over six bits, so at most three passes.
    pub fn reach(&self, entry: u8) -> u8 {
        let mut reach = entry & ALL_FACES;
        loop {
            let mut add = 0u8;
            let mut rest = reach;
            while rest != 0 {
                let f = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                add |= self.adj[f];
            }
            let next = reach | add;
            if next == reach {
                return reach;
            }
            reach = next;
        }
    }
}

impl std::fmt::Debug for SectionGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (mut opens, mut airs, mut links) = (Vec::new(), Vec::new(), Vec::new());
        for a in 0..6u8 {
            if self.is_open(a) {
                opens.push(a);
            }
            if self.is_air(a) {
                airs.push(a);
            }
            for b in 0..6u8 {
                if self.connects(a, b) {
                    links.push((a, b));
                }
            }
        }
        write!(
            f,
            "SectionGraph(open={opens:?}, air={airs:?}, links={links:?})"
        )
    }
}

/// Compute the face graph of one section.
///
/// `blocks` is indexed `x | z << 4 | y << 8` (the layout
/// [`BlockSource`](crate::registry) encoders use). `is_opaque` says whether a
/// block stops sight (a face with a non-opaque cell is *visible*); `is_air`
/// says whether a block is passable, so a section made only of opaque blocks is
/// [`SectionGraph::sealed`] and one whose only non-opaque cells are fluid is
/// visible but does not connect.
pub fn compute_section_graph(
    blocks: &[BlockStateId; SECTION_VOLUME],
    is_opaque: impl Fn(BlockStateId) -> bool,
    is_air: impl Fn(BlockStateId) -> bool,
) -> SectionGraph {
    // Keep the tests out of the inner loop by precomputing them; the flood fill
    // then only ever looks at a `bool`.
    let mut opaque = vec![false; SECTION_VOLUME];
    let mut air = vec![false; SECTION_VOLUME];
    for (i, b) in blocks.iter().enumerate() {
        opaque[i] = is_opaque(*b);
        air[i] = is_air(*b);
    }

    // Which faces have at least one non-opaque cell — the visible surface. A
    // separate pass, because the air flood below never visits a fluid cell.
    let mut open = 0u8;
    for (i, &is_opaque) in opaque.iter().enumerate() {
        if is_opaque {
            continue;
        }
        let x = (i & 15) as i32;
        let z = ((i >> 4) & 15) as i32;
        let y = (i >> 8) as i32;
        if x == 0 {
            open |= 1 << FACE_NEG_X;
        }
        if x == 15 {
            open |= 1 << FACE_POS_X;
        }
        if y == 0 {
            open |= 1 << FACE_NEG_Y;
        }
        if y == 15 {
            open |= 1 << FACE_POS_Y;
        }
        if z == 0 {
            open |= 1 << FACE_NEG_Z;
        }
        if z == 15 {
            open |= 1 << FACE_POS_Z;
        }
    }

    let mut seen = [0u64; SECTION_VOLUME / 64];
    let mut adj = [0u8; 6];
    let mut airs = 0u8;

    for start in 0..SECTION_VOLUME {
        if !air[start] || seen[start >> 6] >> (start & 63) & 1 != 0 {
            continue;
        }
        // Flood fill this pocket of passable (air-like) cells, collecting the
        // faces it touches.
        let mut faces = 0u8;
        let mut queue = VecDeque::new();
        seen[start >> 6] |= 1u64 << (start & 63);
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            let x = (i & 15) as i32;
            let z = ((i >> 4) & 15) as i32;
            let y = (i >> 8) as i32;
            if x == 0 {
                faces |= 1 << FACE_NEG_X;
            }
            if x == 15 {
                faces |= 1 << FACE_POS_X;
            }
            if y == 0 {
                faces |= 1 << FACE_NEG_Y;
            }
            if y == 15 {
                faces |= 1 << FACE_POS_Y;
            }
            if z == 0 {
                faces |= 1 << FACE_NEG_Z;
            }
            if z == 15 {
                faces |= 1 << FACE_POS_Z;
            }
            for d in 0..6u8 {
                let (dx, dy, dz) = face_offset(d);
                let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                if !(0..16).contains(&nx) || !(0..16).contains(&ny) || !(0..16).contains(&nz) {
                    continue;
                }
                let n = (nx | nz << 4 | ny << 8) as usize;
                if !air[n] || seen[n >> 6] >> (n & 63) & 1 != 0 {
                    continue;
                }
                seen[n >> 6] |= 1u64 << (n & 63);
                queue.push_back(n);
            }
        }
        airs |= faces;
        for (f, row) in adj.iter_mut().enumerate() {
            if faces & (1 << f) != 0 {
                *row |= faces;
            }
        }
    }

    SectionGraph {
        open,
        air: airs,
        adj,
        full_clear: opaque.iter().all(|o| !o),
    }
}

/// Whether a block id is opaque for the purposes of the flood fill.
///
/// Engine states carry `solid` ("blocks all light / a full opaque cube"); a
/// state the generated table does not know — a modded block — is treated as
/// opaque, the safe direction: an unknown block must not become a hole in the
/// mask.
pub fn default_is_opaque(id: BlockStateId) -> bool {
    crate::registry::blocks::props_of_state(id)
        .map(|p| p.solid)
        .unwrap_or(true)
}

/// Whether a block id is passable (air-like) for the reachability search.
///
/// Air, cave air and void air are; fluid is **not**, so the search cannot walk
/// through a pool or an ocean into the caves it connects — the leak that let a
/// player over water see flooded caves. Glass and leaves are not (they are not
/// air), so a window is visible but opaque to the search. An unknown block is
/// treated as not passable, the safe direction.
pub fn default_is_air(id: BlockStateId) -> bool {
    crate::registry::blocks::props_of_state(id)
        .map(|p| !p.solid && !p.collision && p.light_opacity == 0)
        .unwrap_or(false)
}

/// Run the per-player visibility BFS.
///
/// Seeds are every section whose *blocks* lie within `margin_blocks` of the
/// player (the "could see it if he turned or moved a little" sphere) and the
/// search expands only through **air** faces a section actually connects,
/// bounded horizontally by `view_distance` sections and vertically by
/// `[y_min, y_max]`. A neighbour whose facing side is non-opaque is *visible*
/// (you can see stone, water, glass) but only an air passage lets the search
/// continue into it, so a pool the player looks at cannot reveal the flooded
/// caves it drains into. A fully opaque face marks nothing — the whole
/// hidden section stays hidden, or a whole section sent real would leak every
/// ore in it. A wave then passes *visibility* through fully-clear sections
/// (all air or all water) so an ocean stays water while its stone floor stops
/// the wave. Direction is deliberately not an input: culling by where the
/// player looks is what makes turning the head show a hole.
///
/// `graph` returns the section graph for a section coordinate, or `None` when
/// that section is outside the world. It should be cached by the caller — the
/// search asks for the same section more than once.
pub fn reachable_sections(
    player: (f64, f64, f64),
    margin_blocks: f64,
    view_distance: i32,
    y_min: i32,
    y_max: i32,
    mut graph: impl FnMut(i32, i32, i32) -> Option<SectionGraph>,
) -> HashSet<(i32, i32, i32)> {
    let floor_section = |v: f64| (v.floor() as i32) >> 4;
    let psx = floor_section(player.0);
    let psz = floor_section(player.2);

    // The seed box in section coordinates, derived from the block-space
    // margin so a player near a border seeds both sides.
    let sx0 = floor_section(player.0 - margin_blocks);
    let sx1 = floor_section(player.0 + margin_blocks);
    let sy0 = floor_section(player.1 - margin_blocks).max(y_min);
    let sy1 = floor_section(player.1 + margin_blocks).min(y_max);
    let sz0 = floor_section(player.2 - margin_blocks);
    let sz1 = floor_section(player.2 + margin_blocks);
    let in_view = |s: (i32, i32, i32)| {
        (s.0 - psx).abs() <= view_distance
            && (s.2 - psz).abs() <= view_distance
            && (y_min..=y_max).contains(&s.1)
    };

    let mut visited: HashMap<(i32, i32, i32), u8> = HashMap::new();
    let mut queue: VecDeque<((i32, i32, i32), u8)> = VecDeque::new();

    for sx in sx0..=sx1 {
        for sy in sy0..=sy1 {
            for sz in sz0..=sz1 {
                let key = (sx, sy, sz);
                if !in_view(key) || visited.contains_key(&key) {
                    continue;
                }
                let Some(g) = graph(sx, sy, sz) else {
                    continue;
                };
                let entry = g.air_faces();
                visited.insert(key, entry);
                queue.push_back((key, entry));
            }
        }
    }

    while let Some((sec, entry)) = queue.pop_front() {
        let Some(g) = graph(sec.0, sec.1, sec.2) else {
            continue;
        };
        let reach = g.reach(entry);
        let mut faces = reach;
        while faces != 0 {
            let f = faces.trailing_zeros() as u8;
            faces &= faces - 1;
            let (dx, dy, dz) = face_offset(f);
            let n = (sec.0 + dx, sec.1 + dy, sec.2 + dz);
            if !in_view(n) {
                continue;
            }
            let Some(ng) = graph(n.0, n.1, n.2) else {
                continue;
            };
            let entry_face = opposite(f);
            // The neighbour is visible only if it has a non-opaque cell on the
            // facing side (you can see stone, water, a window). A fully opaque
            // face is dark rock, so the whole section stays hidden — the shell
            // leak that sent every ore one section away from open space.
            if ng.is_open(entry_face) {
                visited.entry(n).or_insert(0);
            }
            // Only an air passage lets the search continue into the neighbour;
            // a fluid or glass surface is visible but does not connect, so a
            // pool cannot reveal the caves it drains into.
            if !ng.is_air(entry_face) {
                continue;
            }
            let bit = 1u8 << entry_face;
            let seen = visited.entry(n).or_insert(0);
            if *seen & bit != 0 {
                continue;
            }
            *seen |= bit;
            queue.push_back((n, bit));
        }
    }

    // Visibility wave: a fully clear section (all air, or all water — an ocean
    // or a lake) that is visible lets you see the clear section beyond it too.
    // It stops at any section with a single opaque cell, so the ocean stays
    // water but its stone floor, and the stone walls of a flooded cave, are
    // not revealed.
    let mut surface: VecDeque<(i32, i32, i32)> = visited
        .iter()
        .filter(|(key, _)| graph(key.0, key.1, key.2).is_some_and(|g| g.is_full_clear()))
        .map(|(key, _)| *key)
        .collect();
    while let Some(sec) = surface.pop_front() {
        let Some(g) = graph(sec.0, sec.1, sec.2) else {
            continue;
        };
        let mut faces = g.open_faces();
        while faces != 0 {
            let f = faces.trailing_zeros() as u8;
            faces &= faces - 1;
            let (dx, dy, dz) = face_offset(f);
            let n = (sec.0 + dx, sec.1 + dy, sec.2 + dz);
            if !in_view(n) || visited.contains_key(&n) {
                continue;
            }
            let Some(ng) = graph(n.0, n.1, n.2) else {
                continue;
            };
            if !ng.is_open(opposite(f)) {
                continue;
            }
            visited.insert(n, 0);
            if ng.is_full_clear() {
                surface.push_back(n);
            }
        }
    }

    visited.into_keys().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockStateId;

    /// An engine id we treat as opaque / transparent in these tests.
    const STONE: BlockStateId = BlockStateId(1);
    const AIR: BlockStateId = BlockStateId(0);

    fn is_opaque(id: BlockStateId) -> bool {
        id == STONE
    }

    fn is_air(id: BlockStateId) -> bool {
        id == AIR
    }

    fn idx(x: i32, y: i32, z: i32) -> usize {
        (x | z << 4 | y << 8) as usize
    }

    fn solid() -> [BlockStateId; SECTION_VOLUME] {
        [STONE; SECTION_VOLUME]
    }

    #[test]
    fn a_sealed_box_has_no_open_face_and_connects_nothing() {
        let blocks = solid();
        let g = compute_section_graph(&blocks, is_opaque, is_air);
        assert_eq!(g.open_faces(), 0);
        assert_eq!(g.reach(g.open_faces()), 0);
        for a in 0..6 {
            assert!(!g.is_open(a));
        }
    }

    #[test]
    fn all_air_is_fully_open() {
        let blocks = [AIR; SECTION_VOLUME];
        let g = compute_section_graph(&blocks, is_opaque, is_air);
        assert_eq!(g.open_faces(), ALL_FACES);
        assert_eq!(g.reach(1 << FACE_NEG_X), ALL_FACES);
    }

    #[test]
    fn a_tunnel_through_connects_the_two_faces_at_its_ends() {
        let mut blocks = solid();
        for x in 0..16 {
            blocks[idx(x, 8, 8)] = AIR;
        }
        let g = compute_section_graph(&blocks, is_opaque, is_air);
        assert!(g.is_open(FACE_NEG_X) && g.is_open(FACE_POS_X));
        assert!(g.connects(FACE_NEG_X, FACE_POS_X));
        assert!(g.connects(FACE_POS_X, FACE_NEG_X));
        assert_eq!(
            g.reach(1 << FACE_NEG_X),
            (1 << FACE_NEG_X) | (1 << FACE_POS_X)
        );
        // The tunnel does not reach any other face.
        assert!(!g.is_open(FACE_POS_Y));
        assert!(!g.is_open(FACE_NEG_Z));
    }

    #[test]
    fn an_l_shaped_cave_connects_three_faces() {
        // A run along +X ending in a run along +Z.
        let mut blocks = solid();
        for x in 0..16 {
            blocks[idx(x, 8, 8)] = AIR;
        }
        for z in 8..16 {
            blocks[idx(15, 8, z)] = AIR;
        }
        let g = compute_section_graph(&blocks, is_opaque, is_air);
        assert!(g.is_open(FACE_NEG_X) && g.is_open(FACE_POS_X) && g.is_open(FACE_POS_Z));
        assert!(g.connects(FACE_NEG_X, FACE_POS_Z));
        assert!(g.connects(FACE_POS_Z, FACE_NEG_X));
        assert_eq!(
            g.reach(1 << FACE_NEG_X),
            (1 << FACE_NEG_X) | (1 << FACE_POS_X) | (1 << FACE_POS_Z)
        );
    }

    #[test]
    fn two_separate_pockets_do_not_connect() {
        // One tunnel along X at y=2, another along Z at y=12: neither sees the
        // other even though both are non-opaque components in one section.
        let mut blocks = solid();
        for x in 0..16 {
            blocks[idx(x, 2, 8)] = AIR;
        }
        for z in 0..16 {
            blocks[idx(8, 12, z)] = AIR;
        }
        let g = compute_section_graph(&blocks, is_opaque, is_air);
        assert!(g.connects(FACE_NEG_X, FACE_POS_X));
        assert!(g.connects(FACE_NEG_Z, FACE_POS_Z));
        assert!(!g.connects(FACE_NEG_X, FACE_NEG_Z));
        assert!(!g.connects(FACE_NEG_X, FACE_POS_Z));
        assert_eq!(
            g.reach(1 << FACE_NEG_X),
            (1 << FACE_NEG_X) | (1 << FACE_POS_X)
        );
    }

    #[test]
    fn a_one_cell_pocket_connects_only_its_own_face() {
        // A single non-opaque cell in the middle touches no boundary face.
        let mut blocks = solid();
        blocks[idx(8, 8, 8)] = AIR;
        let g = compute_section_graph(&blocks, is_opaque, is_air);
        assert_eq!(g.open_faces(), 0);
        assert_eq!(g.reach(g.open_faces()), 0);
    }

    /// A graph provider over a list of open sections (`(x, y, z)`), every
    /// listed section fully open and every other one sealed. Anything outside
    /// `y_min..=y_max` reads as `None`.
    fn provider<'a>(
        open: &'a HashSet<(i32, i32, i32)>,
        y_min: i32,
        y_max: i32,
    ) -> impl FnMut(i32, i32, i32) -> Option<SectionGraph> + 'a {
        move |x, y, z| {
            if !(y_min..=y_max).contains(&y) {
                return None;
            }
            Some(if open.contains(&(x, y, z)) {
                SectionGraph::open()
            } else {
                SectionGraph::sealed()
            })
        }
    }

    #[test]
    fn a_sealed_neighbour_blocks_the_search() {
        // (0,0,0) and (2,0,0) are open, (1,0,0) is sealed: the search reaches
        // (0,0,0) but must not mark the sealed wall visible — a whole section
        // sent real would leak every ore in it — nor pass through to (2,0,0).
        let mut open = HashSet::new();
        open.insert((0, 0, 0));
        open.insert((2, 0, 0));
        let visible = reachable_sections((0.5, 0.5, 0.5), 0.0, 8, -4, 4, provider(&open, -4, 4));
        assert!(visible.contains(&(0, 0, 0)));
        assert!(
            !visible.contains(&(1, 0, 0)),
            "a fully sealed section stays hidden"
        );
        assert!(
            !visible.contains(&(2, 0, 0)),
            "the cave behind the wall must stay hidden"
        );
    }

    #[test]
    fn a_tunnel_of_open_sections_is_fully_reached() {
        let mut open = HashSet::new();
        for x in 0..6 {
            open.insert((x, 0, 0));
        }
        let visible = reachable_sections((0.5, 0.5, 0.5), 0.0, 8, -4, 4, provider(&open, -4, 4));
        for x in 0..6 {
            assert!(visible.contains(&(x, 0, 0)), "tunnel at x={x}");
        }
        // (6,0,0) is the solid cap at the tunnel mouth: sealed, so it is not
        // marked visible and not passed through.
        assert!(!visible.contains(&(6, 0, 0)));
        assert!(!visible.contains(&(7, 0, 0)));
    }

    #[test]
    fn the_margin_seeds_sections_even_through_a_wall() {
        // Everything is sealed except the player's own section; the margin
        // still marks the sections a few blocks away visible.
        let open: HashSet<(i32, i32, i32)> = HashSet::new();
        let visible = reachable_sections((15.5, 0.5, 0.5), 5.0, 8, -4, 4, provider(&open, -4, 4));
        assert!(visible.contains(&(0, 0, 0)));
        assert!(visible.contains(&(0, 0, 0)) && visible.contains(&(1, 0, 0)));
    }

    #[test]
    fn open_surface_hides_nothing_within_view() {
        let mut open = HashSet::new();
        for x in -10..=10 {
            for z in -10..=10 {
                open.insert((x, 1, z));
            }
        }
        let visible = reachable_sections((0.5, 20.5, 0.5), 0.0, 2, -4, 4, provider(&open, -4, 4));
        // Within the radius every section is open, so all are visible; beyond
        // it the view bound stops the search.
        for x in -2..=2 {
            for z in -2..=2 {
                assert!(
                    visible.contains(&(x, 1, z)),
                    "({x},1,{z}) should be visible"
                );
            }
        }
        assert!(!visible.contains(&(3, 1, 0)), "outside the view radius");
    }

    #[test]
    fn the_visible_set_does_not_depend_on_where_the_player_looks() {
        // There is no yaw or pitch parameter, which *is* the guarantee: a
        // 180° turn cannot change the answer.
        let mut open = HashSet::new();
        for x in -3..=3 {
            open.insert((x, 0, 0));
        }
        let a = reachable_sections((0.5, 0.5, 0.5), 2.0, 8, -4, 4, provider(&open, -4, 4));
        let b = reachable_sections((0.5, 0.5, 0.5), 2.0, 8, -4, 4, provider(&open, -4, 4));
        assert_eq!(a, b);
    }

    // ---- water / fluid visibility (live bug AX2) ------------------------

    /// A block id that is neither opaque nor passable: a fluid.
    const WATER: BlockStateId = BlockStateId(2);

    fn water_is_opaque(id: BlockStateId) -> bool {
        id == STONE
    }

    fn water_is_air(id: BlockStateId) -> bool {
        id == AIR
    }

    fn section_of(id: BlockStateId) -> SectionGraph {
        let blocks = [id; SECTION_VOLUME];
        compute_section_graph(&blocks, water_is_opaque, water_is_air)
    }

    fn mixed_water_stone() -> SectionGraph {
        let mut blocks = [WATER; SECTION_VOLUME];
        blocks[idx(8, 8, 8)] = STONE;
        compute_section_graph(&blocks, water_is_opaque, water_is_air)
    }

    #[test]
    fn the_visibility_wave_passes_through_water_but_stops_at_stone() {
        // A chain of all-water sections is visible (an ocean stays water), but
        // a section holding even one stone cell stops the wave — so the stone
        // walls of a flooded cave beyond are not revealed.
        let water = section_of(WATER);
        let stone = section_of(STONE);
        let mixed = mixed_water_stone();
        assert!(water.is_full_clear());
        assert!(!mixed.is_full_clear());
        assert!(water.air_faces() == 0, "water is not air");
        assert!(water.is_open(FACE_POS_X), "water is see-through");

        let graph = move |x: i32, y: i32, _z: i32| {
            if y != 0 {
                return None;
            }
            Some(match x {
                0 | 1 => water,
                2 => mixed,
                _ => stone,
            })
        };
        let visible = reachable_sections((8.0, 8.0, 8.0), 0.0, 8, -4, 4, graph);
        assert!(visible.contains(&(0, 0, 0)), "the seed water section");
        assert!(
            visible.contains(&(1, 0, 0)),
            "the wave reaches the next water"
        );
        assert!(
            visible.contains(&(2, 0, 0)),
            "the mixed section's water face is visible"
        );
        assert!(
            !visible.contains(&(3, 0, 0)),
            "the stone beyond the mixed section stays hidden"
        );
    }
}
