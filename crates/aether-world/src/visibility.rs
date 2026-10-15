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

/// Which faces of one section see each other through non-opaque blocks.
///
/// `open` is the set of faces that have at least one non-opaque cell on them
/// (a door into the section). `adj[f]` is the set of faces reachable from
/// face `f` through the interior, so `adj[f]` always contains `f` when `f` is
/// open. Together they are enough to walk a cave system without ever reading
/// the blocks again.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct SectionGraph {
    open: u8,
    adj: [u8; 6],
}

impl SectionGraph {
    /// A fully opaque section: no face is a way in, nothing connects.
    pub const fn sealed() -> Self {
        Self {
            open: 0,
            adj: [0; 6],
        }
    }

    /// A fully open section: every face is a way in and every face sees every
    /// other — the answer for air, water and every transparent section.
    pub const fn open() -> Self {
        Self {
            open: ALL_FACES,
            adj: [ALL_FACES; 6],
        }
    }

    /// The faces with at least one non-opaque cell.
    #[inline]
    pub fn open_faces(&self) -> u8 {
        self.open
    }

    /// Whether `face` has any non-opaque cell.
    #[inline]
    pub fn is_open(&self, face: u8) -> bool {
        self.open & (1 << face) != 0
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
        let mut opens = Vec::new();
        let mut links = Vec::new();
        for a in 0..6u8 {
            if self.is_open(a) {
                opens.push(a);
            }
            for b in 0..6u8 {
                if self.connects(a, b) {
                    links.push((a, b));
                }
            }
        }
        write!(f, "SectionGraph(open={opens:?}, links={links:?})")
    }
}

/// Compute the face graph of one section.
///
/// `blocks` is indexed `x | z << 4 | y << 8` (the layout
/// [`BlockSource`](crate::registry) encoders use). `is_opaque` says whether a
/// block stops sight; a section made only of opaque blocks is
/// [`SectionGraph::sealed`].
pub fn compute_section_graph(
    blocks: &[BlockStateId; SECTION_VOLUME],
    is_opaque: impl Fn(BlockStateId) -> bool,
) -> SectionGraph {
    // Keep the opaque test out of the inner loop by precomputing it; the
    // flood fill then only ever looks at a `bool`.
    let mut opaque = vec![false; SECTION_VOLUME];
    for (i, b) in blocks.iter().enumerate() {
        opaque[i] = is_opaque(*b);
    }

    let mut seen = [0u64; SECTION_VOLUME / 64];
    let mut adj = [0u8; 6];
    let mut open = 0u8;

    for start in 0..SECTION_VOLUME {
        if opaque[start] || seen[start >> 6] >> (start & 63) & 1 != 0 {
            continue;
        }
        // Flood fill this pocket of non-opaque cells, collecting the faces it
        // touches.
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
                if opaque[n] || seen[n >> 6] >> (n & 63) & 1 != 0 {
                    continue;
                }
                seen[n >> 6] |= 1u64 << (n & 63);
                queue.push_back(n);
            }
        }
        open |= faces;
        for (f, row) in adj.iter_mut().enumerate() {
            if faces & (1 << f) != 0 {
                *row |= faces;
            }
        }
    }

    SectionGraph { open, adj }
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

/// Run the per-player visibility BFS.
///
/// Seeds are every section whose *blocks* lie within `margin_blocks` of the
/// player (the "could see it if he turned or moved a little" sphere) and the
/// search expands only through faces a section actually connects, bounded
/// horizontally by `view_distance` sections and vertically by
/// `[y_min, y_max]`. Direction is deliberately not an input: culling by where
/// the player looks is what makes turning the head show a hole.
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
                let entry = g.open_faces();
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
            // The neighbour's face is geometry the player can see even when
            // it is solid stone, so it becomes visible either way. Only an
            // actual opening lets the search continue through it.
            let entry_face = opposite(f);
            if !ng.is_open(entry_face) {
                visited.entry(n).or_insert(0);
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

    fn idx(x: i32, y: i32, z: i32) -> usize {
        (x | z << 4 | y << 8) as usize
    }

    fn solid() -> [BlockStateId; SECTION_VOLUME] {
        [STONE; SECTION_VOLUME]
    }

    #[test]
    fn a_sealed_box_has_no_open_face_and_connects_nothing() {
        let blocks = solid();
        let g = compute_section_graph(&blocks, is_opaque);
        assert_eq!(g.open_faces(), 0);
        assert_eq!(g.reach(g.open_faces()), 0);
        for a in 0..6 {
            assert!(!g.is_open(a));
        }
    }

    #[test]
    fn all_air_is_fully_open() {
        let blocks = [AIR; SECTION_VOLUME];
        let g = compute_section_graph(&blocks, is_opaque);
        assert_eq!(g.open_faces(), ALL_FACES);
        assert_eq!(g.reach(1 << FACE_NEG_X), ALL_FACES);
    }

    #[test]
    fn a_tunnel_through_connects_the_two_faces_at_its_ends() {
        let mut blocks = solid();
        for x in 0..16 {
            blocks[idx(x, 8, 8)] = AIR;
        }
        let g = compute_section_graph(&blocks, is_opaque);
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
        let g = compute_section_graph(&blocks, is_opaque);
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
        let g = compute_section_graph(&blocks, is_opaque);
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
        let g = compute_section_graph(&blocks, is_opaque);
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
        // (0,0,0) and (2,0,0) are open, (1,0,0) is sealed: the search can
        // reach (1,0,0) (its face is open, so the stone is visible) but must
        // not pass through it to the cave at (2,0,0).
        let mut open = HashSet::new();
        open.insert((0, 0, 0));
        open.insert((2, 0, 0));
        let visible = reachable_sections((0.5, 0.5, 0.5), 0.0, 8, -4, 4, provider(&open, -4, 4));
        assert!(visible.contains(&(0, 0, 0)));
        assert!(
            visible.contains(&(1, 0, 0)),
            "the sealed section's face is visible"
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
        // (6,0,0) is the solid cap at the tunnel mouth: its face is visible
        // (the player sees stone), but the search does not pass through it.
        assert!(visible.contains(&(6, 0, 0)));
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
}
