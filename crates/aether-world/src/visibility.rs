//! Section visibility graph and the per-player reachable set — the pure half
//! of the optional anti-xray / visibility-culling module
//! (`docs/design/visibility-culling.md`).
//!
//! Nothing here talks to a client or to the network. Given the blocks of one
//! 16³ section it computes which of the section's six faces see each other
//! through non-opaque space. It is deterministic and side-effect free, so the
//! decision can be tested without a socket.
//!
//! The graph is the same idea as the vanilla client's own `VisGraph`: a flood
//! fill over the non-opaque cells of a section, recording which boundary faces
//! each connected pocket touches. Two faces of the same pocket are connected,
//! which is exactly "you can enter here and see out there".

use std::collections::VecDeque;

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
}
