//! Section visibility graph and the per-player reachable set — the pure half
//! of the optional anti-xray / visibility-culling module
//! (`docs/design/visibility-culling.md`).
//!
//! Nothing here talks to a client or to the network. Given the blocks of one
//! box — a 16³ section or a `4×4×2` AVX-cell — it computes which of the box's
//! six faces see each other through non-opaque space, and given a way to ask
//! for a box's graph it runs the breadth-first search a server uses to decide
//! which boxes a player can see. Both are deterministic and side-effect free,
//! so the decisions can be tested without a socket.
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

/// The dimensions of a rectangular block box the graph and search operate on.
///
/// [`BoxDims::SECTION`] is the classic `16×16×16` section; [`BoxDims::CELL`] is
/// the AVX-cell `4×4×2` box (see [`crate::cell::AvxCell`]). Every extent is a
/// power of two, which the local index relies on.
///
/// The local index of a block stays the existing shape
/// `x | z << log2(x) | y << log2(x*z)` — for a section `x | z << 4 | y << 8`,
/// for a `4×4×2` cell `x | z << 2 | y << 4` — so nothing about the section
/// layout changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BoxDims {
    /// Blocks along X.
    pub x: usize,
    /// Blocks along Y.
    pub y: usize,
    /// Blocks along Z.
    pub z: usize,
}

impl BoxDims {
    /// A `16×16×16` section.
    pub const SECTION: BoxDims = BoxDims {
        x: 16,
        y: 16,
        z: 16,
    };
    /// A `4×4×2` AVX-cell, matching [`crate::cell::AvxCell`].
    pub const CELL: BoxDims = BoxDims { x: 4, y: 2, z: 4 };

    /// The number of blocks in the box.
    #[inline]
    pub const fn volume(self) -> usize {
        self.x * self.y * self.z
    }
}

/// Local index of `(x, y, z)` in a box of `dims`:
/// `x | z << log2(dims.x) | y << log2(dims.x * dims.z)`.
#[inline]
fn local_index(dims: BoxDims, x: usize, y: usize, z: usize) -> usize {
    x | (z << dims.x.trailing_zeros()) | (y << (dims.x * dims.z).trailing_zeros())
}

/// Floor-division of a world coordinate by one axis' box extent.
#[inline]
fn floordiv(v: f64, d: usize) -> i32 {
    (v.floor() as i32).div_euclid(d as i32)
}

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

/// Compute the face graph of one rectangular box of blocks.
///
/// `blocks` is indexed `x | z << log2(dims.x) | y << log2(dims.x * dims.z)` —
/// for a section the old `x | z << 4 | y << 8` (the layout
/// [`BlockSource`](crate::registry) encoders use). `is_opaque` says whether a
/// block stops sight (a face with a non-opaque cell is *visible*); `is_air`
/// says whether a block is passable, so a box made only of opaque blocks is
/// [`SectionGraph::sealed`] and one whose only non-opaque cells are fluid is
/// visible but does not connect.
///
/// [`BoxDims::SECTION`] reproduces the section graph block for block;
/// [`BoxDims::CELL`] runs the same flood fill on a `4×4×2` AVX-cell.
pub fn compute_box_graph(
    dims: BoxDims,
    blocks: &[BlockStateId],
    is_opaque: impl Fn(BlockStateId) -> bool,
    is_air: impl Fn(BlockStateId) -> bool,
) -> SectionGraph {
    let volume = dims.volume();

    // Keep the tests out of the inner loop by precomputing them; the flood fill
    // then only ever looks at a `bool`.
    let mut opaque = vec![false; volume];
    let mut air = vec![false; volume];
    for (i, b) in blocks.iter().enumerate() {
        opaque[i] = is_opaque(*b);
        air[i] = is_air(*b);
    }

    let x_mask = dims.x - 1;
    let z_mask = dims.z - 1;
    let x_shift = dims.x.trailing_zeros();
    let y_shift = (dims.x * dims.z).trailing_zeros();
    let max_x = dims.x as i32 - 1;
    let max_y = dims.y as i32 - 1;
    let max_z = dims.z as i32 - 1;

    // Which faces have at least one non-opaque cell — the visible surface. A
    // separate pass, because the air flood below never visits a fluid cell.
    let mut open = 0u8;
    for (i, &is_opaque) in opaque.iter().enumerate() {
        if is_opaque {
            continue;
        }
        let x = (i & x_mask) as i32;
        let z = ((i >> x_shift) & z_mask) as i32;
        let y = (i >> y_shift) as i32;
        if x == 0 {
            open |= 1 << FACE_NEG_X;
        }
        if x == max_x {
            open |= 1 << FACE_POS_X;
        }
        if y == 0 {
            open |= 1 << FACE_NEG_Y;
        }
        if y == max_y {
            open |= 1 << FACE_POS_Y;
        }
        if z == 0 {
            open |= 1 << FACE_NEG_Z;
        }
        if z == max_z {
            open |= 1 << FACE_POS_Z;
        }
    }

    let mut seen = vec![0u64; volume.div_ceil(64)];
    let mut adj = [0u8; 6];
    let mut airs = 0u8;

    for start in 0..volume {
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
            let x = (i & x_mask) as i32;
            let z = ((i >> x_shift) & z_mask) as i32;
            let y = (i >> y_shift) as i32;
            if x == 0 {
                faces |= 1 << FACE_NEG_X;
            }
            if x == max_x {
                faces |= 1 << FACE_POS_X;
            }
            if y == 0 {
                faces |= 1 << FACE_NEG_Y;
            }
            if y == max_y {
                faces |= 1 << FACE_POS_Y;
            }
            if z == 0 {
                faces |= 1 << FACE_NEG_Z;
            }
            if z == max_z {
                faces |= 1 << FACE_POS_Z;
            }
            for d in 0..6u8 {
                let (dx, dy, dz) = face_offset(d);
                let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                if !(0..dims.x as i32).contains(&nx)
                    || !(0..dims.y as i32).contains(&ny)
                    || !(0..dims.z as i32).contains(&nz)
                {
                    continue;
                }
                let n = local_index(dims, nx as usize, ny as usize, nz as usize);
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

/// Compute the face graph of one `16³` section.
///
/// Thin wrapper over [`compute_box_graph`] with [`BoxDims::SECTION`].
pub fn compute_section_graph(
    blocks: &[BlockStateId; SECTION_VOLUME],
    is_opaque: impl Fn(BlockStateId) -> bool,
    is_air: impl Fn(BlockStateId) -> bool,
) -> SectionGraph {
    compute_box_graph(BoxDims::SECTION, blocks.as_slice(), is_opaque, is_air)
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

/// Run the per-player visibility BFS over boxes of size `dims`.
///
/// Seeds are every box whose *blocks* lie within `margin_blocks` of the player
/// (the "could see it if he turned or moved a little" sphere) and the search
/// expands only through **air** faces a box actually connects, bounded
/// horizontally by `view_distance` boxes and vertically by `[y_min, y_max]`. A
/// neighbour whose facing side is non-opaque is *visible* (you can see stone,
/// water, glass) but only an air passage lets the search continue into it, so a
/// pool the player looks at cannot reveal the flooded caves it drains into. A
/// fully opaque face marks nothing — the whole hidden box stays hidden, or a
/// whole box sent real would leak every ore in it. A wave then passes
/// *visibility* through fully-clear boxes (all air or all water) so an ocean
/// stays water while its stone floor stops the wave. Direction is deliberately
/// not an input: culling by where the player looks is what makes turning the
/// head show a hole.
///
/// `graph` returns the box graph for a box coordinate, or `None` when that box
/// is outside the world. It should be cached by the caller — the search asks
/// for the same box more than once. The only change from the section-only
/// search is that the player and margin coordinates are floor-divided per axis
/// by `dims`, so a `4×4×2` cell divides X/Z by 4 and Y by 2.
pub fn reachable_boxes(
    dims: BoxDims,
    player: (f64, f64, f64),
    margin_blocks: f64,
    view_distance: i32,
    y_min: i32,
    y_max: i32,
    mut graph: impl FnMut(i32, i32, i32) -> Option<SectionGraph>,
) -> HashSet<(i32, i32, i32)> {
    let psx = floordiv(player.0, dims.x);
    let psy = floordiv(player.1, dims.y);
    let psz = floordiv(player.2, dims.z);
    let player_box = (psx, psy, psz);

    // The seed box in box coordinates, derived from the block-space margin so a
    // player near a border seeds both sides.
    let sx0 = floordiv(player.0 - margin_blocks, dims.x);
    let sx1 = floordiv(player.0 + margin_blocks, dims.x);
    let sy0 = floordiv(player.1 - margin_blocks, dims.y).max(y_min);
    let sy1 = floordiv(player.1 + margin_blocks, dims.y).min(y_max);
    let sz0 = floordiv(player.2 - margin_blocks, dims.z);
    let sz1 = floordiv(player.2 + margin_blocks, dims.z);
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
                // A fully opaque section cannot be seen into: sending it whole
                // as real blocks leaks every ore it holds, so it must stay
                // hidden. Only the section the player actually stands in is
                // sent even when sealed. This is what made the margin reveal
                // the solid shell around the player and its caves.
                if g.open_faces() == 0 && key != player_box {
                    continue;
                }
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

/// Run the per-player visibility BFS over `16³` sections.
///
/// Thin wrapper over [`reachable_boxes`] with [`BoxDims::SECTION`], preserving
/// the original section-typed entry point.
pub fn reachable_sections(
    player: (f64, f64, f64),
    margin_blocks: f64,
    view_distance: i32,
    y_min: i32,
    y_max: i32,
    graph: impl FnMut(i32, i32, i32) -> Option<SectionGraph>,
) -> HashSet<(i32, i32, i32)> {
    reachable_boxes(
        BoxDims::SECTION,
        player,
        margin_blocks,
        view_distance,
        y_min,
        y_max,
        graph,
    )
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
    fn the_margin_never_reveals_a_sealed_section() {
        // Everything is sealed. The section the player stands in is sent (he
        // is inside it), but a sealed section a few blocks away — even inside
        // the margin — must stay hidden, or sending it whole would leak every
        // ore in it. This was the "shell around the player / its caves" leak.
        let open: HashSet<(i32, i32, i32)> = HashSet::new();
        let visible = reachable_sections((15.5, 0.5, 0.5), 5.0, 8, -4, 4, provider(&open, -4, 4));
        assert!(visible.contains(&(0, 0, 0)), "the player's own section");
        assert!(
            !visible.contains(&(1, 0, 0)),
            "a sealed section inside the margin stays hidden"
        );
        assert!(
            !visible.contains(&(0, -1, 0)) && !visible.contains(&(0, 1, 0)),
            "sealed sections above and below stay hidden"
        );
    }

    #[test]
    fn the_margin_keeps_air_sections_near_the_player_visible() {
        // A section with air inside the margin is visible even if no air face
        // connects it to the player's section, so turning or stepping does not
        // pop it in.
        let mut open = HashSet::new();
        open.insert((0, 0, 0));
        open.insert((0, 1, 0)); // directly above, within margin
        let visible = reachable_sections((15.5, 0.5, 0.5), 5.0, 8, -4, 4, provider(&open, -4, 4));
        assert!(visible.contains(&(0, 0, 0)));
        assert!(visible.contains(&(0, 1, 0)));
    }

    #[test]
    fn a_player_sealed_in_stone_sends_only_its_own_section() {
        // The player is in a one-cell air pocket; only his own section may be
        // visible. Everything around it is solid rock and must be stone.
        let graph = |x: i32, y: i32, z: i32| -> Option<SectionGraph> {
            if !(-4..=4).contains(&y) {
                return None;
            }
            Some(if (x, y, z) == (0, 0, 0) {
                let mut blocks = [STONE; SECTION_VOLUME];
                blocks[idx(0, 0, 0)] = AIR;
                compute_section_graph(&blocks, is_opaque, is_air)
            } else {
                SectionGraph::sealed()
            })
        };
        let visible = reachable_sections((0.5, 0.5, 0.5), 5.0, 8, -4, 4, graph);
        assert!(visible.contains(&(0, 0, 0)), "the pocket the player is in");
        assert!(
            !visible.contains(&(0, -1, 0)) && !visible.contains(&(1, 0, 0)),
            "surrounding sealed sections stay hidden: {visible:?}"
        );
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

    // ---- AX3a: generalised box graph and BFS (4×4×2 AVX-cell) ----------

    /// Local index of a block in a 4×4×2 cell: `x | z << 2 | y << 4`.
    fn cidx(x: i32, y: i32, z: i32) -> usize {
        local_index(BoxDims::CELL, x as usize, y as usize, z as usize)
    }

    #[test]
    fn cell_dims_match_avx_cell() {
        assert_eq!(BoxDims::CELL.x, crate::cell::AvxCell::DIM_X);
        assert_eq!(BoxDims::CELL.y, crate::cell::AvxCell::DIM_Y);
        assert_eq!(BoxDims::CELL.z, crate::cell::AvxCell::DIM_Z);
        assert_eq!(BoxDims::CELL.volume(), crate::cell::AvxCell::LEN);
    }

    #[test]
    fn local_index_keeps_the_section_shape_and_fits_the_cell() {
        assert_eq!(
            local_index(BoxDims::SECTION, 15, 15, 15),
            15 | 15 << 4 | 15 << 8
        );
        assert_eq!(local_index(BoxDims::CELL, 3, 1, 3), 3 | 3 << 2 | 1 << 4);
    }

    #[test]
    fn an_all_air_cell_is_fully_open() {
        let blocks = [AIR; 32];
        let g = compute_box_graph(BoxDims::CELL, &blocks, is_opaque, is_air);
        assert_eq!(g, SectionGraph::open());
    }

    #[test]
    fn a_solid_cell_is_sealed() {
        let blocks = [STONE; 32];
        let g = compute_box_graph(BoxDims::CELL, &blocks, is_opaque, is_air);
        assert_eq!(g, SectionGraph::sealed());
    }

    #[test]
    fn a_cell_with_a_neg_y_opening_does_not_reach_pos_y() {
        // A single air cell on the −Y face: that face is open and passable,
        // but the stone above stops the flood short of +Y.
        let mut blocks = [STONE; 32];
        blocks[cidx(1, 0, 1)] = AIR;
        let g = compute_box_graph(BoxDims::CELL, &blocks, is_opaque, is_air);
        assert!(g.air_faces() & (1 << FACE_NEG_Y) != 0);
        assert!(g.open_faces() & (1 << FACE_NEG_Y) != 0);
        assert_eq!(g.reach(1 << FACE_NEG_Y), 1 << FACE_NEG_Y);
        assert_eq!(g.reach(1 << FACE_NEG_Y) & (1 << FACE_POS_Y), 0);
    }

    /// A graph provider over boxes: listed boxes are fully open, the rest
    /// sealed; anything outside `y_min..=y_max` reads as `None`.
    fn box_provider<'a>(
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
    fn reachable_boxes_hides_a_sealed_cell_and_keeps_the_players_cell() {
        let mut open = HashSet::new();
        open.insert((0, 0, 0));
        open.insert((2, 0, 0));
        let visible = reachable_boxes(
            BoxDims::CELL,
            (0.5, 0.5, 0.5),
            0.0,
            8,
            -4,
            4,
            box_provider(&open, -4, 4),
        );
        assert!(visible.contains(&(0, 0, 0)), "the player's own cell");
        assert!(
            !visible.contains(&(1, 0, 0)),
            "a fully sealed cell stays hidden"
        );
        assert!(
            !visible.contains(&(2, 0, 0)),
            "the cell behind the wall stays hidden"
        );
    }

    #[test]
    fn reachable_boxes_wave_passes_through_a_clear_cell() {
        let mut open = HashSet::new();
        for x in 0..3 {
            open.insert((x, 0, 0));
        }
        let visible = reachable_boxes(
            BoxDims::CELL,
            (0.5, 0.5, 0.5),
            0.0,
            8,
            -4,
            4,
            box_provider(&open, -4, 4),
        );
        for x in 0..3 {
            assert!(visible.contains(&(x, 0, 0)), "clear cell at x={x}");
        }
    }

    #[test]
    fn cell_and_section_search_agree_on_an_all_air_world() {
        // On a world of only open boxes the coordinate maths is the only
        // difference, so the two granularities must return the same boxes.
        let all = |_x: i32, _y: i32, _z: i32| Some(SectionGraph::open());
        let cell = reachable_boxes(BoxDims::CELL, (0.5, 0.5, 0.5), 0.0, 2, -2, 2, all);
        let section = reachable_boxes(BoxDims::SECTION, (0.5, 0.5, 0.5), 0.0, 2, -2, 2, all);
        assert_eq!(cell, section);
        assert!(!cell.is_empty());
    }
}
