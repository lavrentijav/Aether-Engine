//! The world as decoration sees it — vanilla's `WorldGenRegion`: the chunk
//! being decorated plus its eight neighbours, read and written by block
//! position, with the heightmaps the placement modifiers query.

use std::cell::RefCell;
use std::sync::Arc;

use aether_world::BlockStateId;

use super::super::biome::BiomeId;
use super::super::biome_manager;
use super::super::blockinfo;
use super::super::chunk::ProtoChunk;
use super::super::generator::Core;
use super::super::random::XoroshiroRandom;

/// A block position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pos {
    /// X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Z.
    pub z: i32,
}

impl Pos {
    /// A position.
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
    /// Offset by a vector.
    pub fn offset(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }
    /// `above(n)`.
    pub fn above(self, n: i32) -> Self {
        self.offset(0, n, 0)
    }
    /// `below(n)`.
    pub fn below(self, n: i32) -> Self {
        self.offset(0, -n, 0)
    }
    /// One step in a direction.
    pub fn rel(self, d: Dir) -> Self {
        let (x, y, z) = d.step();
        self.offset(x, y, z)
    }
    /// `n` steps in a direction.
    pub fn rel_n(self, d: Dir, n: i32) -> Self {
        let (x, y, z) = d.step();
        self.offset(x * n, y * n, z * n)
    }
    /// `distManhattan`.
    pub fn manhattan(self, o: Pos) -> i32 {
        (self.x - o.x).abs() + (self.y - o.y).abs() + (self.z - o.z).abs()
    }
    /// `Vec3i.hashCode`.
    pub fn java_hash(self) -> i32 {
        (self.y.wrapping_add(self.z.wrapping_mul(31)))
            .wrapping_mul(31)
            .wrapping_add(self.x)
    }
}

/// `Direction`, in the game's declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    /// -Y.
    Down,
    /// +Y.
    Up,
    /// -Z.
    North,
    /// +Z.
    South,
    /// -X.
    West,
    /// +X.
    East,
}

impl Dir {
    /// `Direction.values()`.
    pub const ALL: [Dir; 6] = [
        Dir::Down,
        Dir::Up,
        Dir::North,
        Dir::South,
        Dir::West,
        Dir::East,
    ];
    /// `Direction.Plane.HORIZONTAL`, in its iteration order.
    pub const HORIZONTAL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

    /// The unit step.
    pub fn step(self) -> (i32, i32, i32) {
        match self {
            Dir::Down => (0, -1, 0),
            Dir::Up => (0, 1, 0),
            Dir::North => (0, 0, -1),
            Dir::South => (0, 0, 1),
            Dir::West => (-1, 0, 0),
            Dir::East => (1, 0, 0),
        }
    }
    /// The opposite direction.
    pub fn opposite(self) -> Dir {
        match self {
            Dir::Down => Dir::Up,
            Dir::Up => Dir::Down,
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::West => Dir::East,
            Dir::East => Dir::West,
        }
    }
    /// `getClockWise` (horizontal only).
    pub fn clockwise(self) -> Dir {
        match self {
            Dir::North => Dir::East,
            Dir::East => Dir::South,
            Dir::South => Dir::West,
            Dir::West => Dir::North,
            d => d,
        }
    }
    /// The axis name (`x`, `y`, `z`).
    pub fn axis(self) -> &'static str {
        match self {
            Dir::Down | Dir::Up => "y",
            Dir::North | Dir::South => "z",
            Dir::West | Dir::East => "x",
        }
    }
    /// Whether the axis direction is positive.
    pub fn positive(self) -> bool {
        matches!(self, Dir::Up | Dir::South | Dir::East)
    }
    /// The lower-case name (`north`, …).
    pub fn name(self) -> &'static str {
        match self {
            Dir::Down => "down",
            Dir::Up => "up",
            Dir::North => "north",
            Dir::South => "south",
            Dir::West => "west",
            Dir::East => "east",
        }
    }
    /// Parse a lower-case name.
    pub fn parse(s: &str) -> Option<Dir> {
        Dir::ALL.into_iter().find(|d| d.name() == s)
    }
}

/// The heightmap types decoration reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heightmap {
    /// `WORLD_SURFACE_WG`: frozen after carving.
    WorldSurfaceWg,
    /// `OCEAN_FLOOR_WG`: frozen after carving.
    OceanFloorWg,
    /// `WORLD_SURFACE`.
    WorldSurface,
    /// `OCEAN_FLOOR`.
    OceanFloor,
    /// `MOTION_BLOCKING`.
    MotionBlocking,
    /// `MOTION_BLOCKING_NO_LEAVES`.
    MotionBlockingNoLeaves,
}

impl Heightmap {
    /// Parse the JSON name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "WORLD_SURFACE_WG" => Heightmap::WorldSurfaceWg,
            "OCEAN_FLOOR_WG" => Heightmap::OceanFloorWg,
            "WORLD_SURFACE" => Heightmap::WorldSurface,
            "OCEAN_FLOOR" => Heightmap::OceanFloor,
            "MOTION_BLOCKING" => Heightmap::MotionBlocking,
            "MOTION_BLOCKING_NO_LEAVES" => Heightmap::MotionBlockingNoLeaves,
            _ => return None,
        })
    }

    fn opaque(self, s: BlockStateId) -> bool {
        match self {
            Heightmap::WorldSurfaceWg | Heightmap::WorldSurface => !blockinfo::is_air(s),
            Heightmap::OceanFloorWg | Heightmap::OceanFloor => blockinfo::blocks_motion(s),
            Heightmap::MotionBlocking => blockinfo::blocks_motion(s) || blockinfo::has_fluid(s),
            Heightmap::MotionBlockingNoLeaves => {
                (blockinfo::blocks_motion(s) || blockinfo::has_fluid(s)) && !blockinfo::is_leaves(s)
            }
        }
    }

    fn live_index(self) -> Option<usize> {
        match self {
            Heightmap::WorldSurface => Some(0),
            Heightmap::OceanFloor => Some(1),
            Heightmap::MotionBlocking => Some(2),
            Heightmap::MotionBlockingNoLeaves => Some(3),
            _ => None,
        }
    }
}

const LIVE: [Heightmap; 4] = [
    Heightmap::WorldSurface,
    Heightmap::OceanFloor,
    Heightmap::MotionBlocking,
    Heightmap::MotionBlockingNoLeaves,
];

struct Slot {
    chunk: ProtoChunk,
    /// First-available heights for the four live types, built on first use.
    live: [Option<Box<[i32; 256]>>; 4],
    /// `WORLD_SURFACE_WG` / `OCEAN_FLOOR_WG` as they stood after carving.
    wg: [Option<Box<[i32; 256]>>; 2],
    base: Arc<ProtoChunk>,
}

/// The 3×3 chunks decoration may touch.
pub struct Level<'a> {
    pub(crate) core: &'a Core,
    cx: i32,
    cz: i32,
    slots: Vec<Slot>,
    writes: Vec<(i32, i32, i32, BlockStateId)>,
    biome_cache: RefCell<super::super::FxHashMap<(i32, i32, i32), BiomeId>>,
    /// `WorldGenRegion.getRandom()`.
    pub region_random: XoroshiroRandom,
}

impl<'a> Level<'a> {
    /// Over the 3×3 chunks around `(cx, cz)`, `region[dz][dx]`.
    pub(crate) fn new(
        core: &'a Core,
        cx: i32,
        cz: i32,
        region: &[[Arc<ProtoChunk>; 3]; 3],
    ) -> Self {
        let mut slots = Vec::with_capacity(9);
        for row in region {
            for c in row {
                slots.push(Slot {
                    chunk: (**c).clone(),
                    live: [None, None, None, None],
                    wg: [None, None],
                    base: Arc::clone(c),
                });
            }
        }
        // `RandomState.getOrCreateRandomFactory("worldgen_region_random").at(center)`;
        // only pale-garden moss carpets read it.
        let rr = core
            .terrain
            .noises()
            .forked_factory("minecraft:worldgen_region_random")
            .at(cx * 16, 0, cz * 16);
        Self {
            core,
            cx,
            cz,
            slots,
            writes: Vec::new(),
            biome_cache: RefCell::new(Default::default()),
            region_random: rr,
        }
    }

    /// The writes made so far, in order.
    pub fn into_writes(self) -> Vec<(i32, i32, i32, BlockStateId)> {
        self.writes
    }

    #[inline]
    fn slot_of(&self, x: i32, z: i32) -> Option<usize> {
        let dx = (x >> 4) - self.cx + 1;
        let dz = (z >> 4) - self.cz + 1;
        if (0..3).contains(&dx) && (0..3).contains(&dz) {
            Some((dz * 3 + dx) as usize)
        } else {
            None
        }
    }

    /// Lowest block Y.
    pub fn min_y(&self) -> i32 {
        self.core.min_y
    }

    /// One past the highest block Y.
    pub fn max_y(&self) -> i32 {
        self.core.min_y + self.core.height
    }

    /// `isOutsideBuildHeight`.
    pub fn outside_height(&self, y: i32) -> bool {
        y < self.min_y() || y >= self.max_y()
    }

    /// The sea level.
    pub fn sea_level(&self) -> i32 {
        self.core.sea_level
    }

    /// `getBlockState`; air outside the region.
    #[inline]
    pub fn get(&self, p: Pos) -> BlockStateId {
        match self.slot_of(p.x, p.z) {
            Some(i) => self.slots[i]
                .chunk
                .get((p.x & 15) as usize, p.y, (p.z & 15) as usize),
            None => BlockStateId::AIR,
        }
    }

    /// `ensureCanWrite`.
    pub fn can_write(&self, p: Pos) -> bool {
        self.slot_of(p.x, p.z).is_some()
    }

    /// `setBlock`: false outside the region or the world.
    pub fn set(&mut self, p: Pos, s: BlockStateId) -> bool {
        self.set_inner(p, s, true)
    }

    /// Set without heightmap upkeep — ore placement writes straight into the
    /// section the way the game's `BulkSectionAccess` does.
    pub fn set_raw(&mut self, p: Pos, s: BlockStateId) -> bool {
        self.set_inner(p, s, false)
    }

    fn set_inner(&mut self, p: Pos, s: BlockStateId, heightmaps: bool) -> bool {
        let Some(i) = self.slot_of(p.x, p.z) else {
            return false;
        };
        if self.outside_height(p.y) {
            return false;
        }
        let (lx, lz) = ((p.x & 15) as usize, (p.z & 15) as usize);
        if heightmaps {
            for (k, ty) in LIVE.iter().enumerate() {
                if self.slots[i].live[k].is_none() {
                    let hm = self.compute(i, *ty);
                    self.slots[i].live[k] = Some(hm);
                }
            }
        }
        self.slots[i].chunk.set(lx, p.y, lz, s);
        if heightmaps {
            for (k, ty) in LIVE.iter().enumerate() {
                let cur = self.slots[i].live[k].as_ref().unwrap()[lz * 16 + lx];
                if let Some(nv) = self.update_height(i, *ty, lx, lz, p.y, s, cur) {
                    self.slots[i].live[k].as_mut().unwrap()[lz * 16 + lx] = nv;
                }
            }
        }
        self.writes.push((p.x, p.y, p.z, s));
        true
    }

    /// `Heightmap.update`.
    #[allow(clippy::too_many_arguments)]
    fn update_height(
        &self,
        i: usize,
        ty: Heightmap,
        lx: usize,
        lz: usize,
        y: i32,
        s: BlockStateId,
        cur: i32,
    ) -> Option<i32> {
        if y <= cur - 2 {
            return None;
        }
        if ty.opaque(s) {
            if y >= cur {
                return Some(y + 1);
            }
        } else if cur - 1 == y {
            let c = &self.slots[i].chunk;
            let mut j = y - 1;
            while j >= c.min_y {
                if ty.opaque(c.get(lx, j, lz)) {
                    return Some(j + 1);
                }
                j -= 1;
            }
            return Some(c.min_y);
        }
        None
    }

    fn compute(&self, i: usize, ty: Heightmap) -> Box<[i32; 256]> {
        let c = match ty {
            Heightmap::WorldSurfaceWg | Heightmap::OceanFloorWg => &*self.slots[i].base,
            _ => &self.slots[i].chunk,
        };
        let mut out = Box::new([0i32; 256]);
        for lz in 0..16 {
            for lx in 0..16 {
                out[lz * 16 + lx] = c.height_where(lx, lz, |s| ty.opaque(s));
            }
        }
        out
    }

    /// `getHeight(type, x, z)`: the first free Y above the column's top
    /// matching block; `min_y` outside the region.
    pub fn height(&mut self, ty: Heightmap, x: i32, z: i32) -> i32 {
        let Some(i) = self.slot_of(x, z) else {
            return self.min_y();
        };
        let idx = ((z & 15) * 16 + (x & 15)) as usize;
        match ty.live_index() {
            Some(k) => {
                if self.slots[i].live[k].is_none() {
                    let hm = self.compute(i, ty);
                    self.slots[i].live[k] = Some(hm);
                }
                self.slots[i].live[k].as_ref().unwrap()[idx]
            }
            None => {
                let k = if ty == Heightmap::WorldSurfaceWg {
                    0
                } else {
                    1
                };
                if self.slots[i].wg[k].is_none() {
                    let hm = self.compute(i, ty);
                    self.slots[i].wg[k] = Some(hm);
                }
                self.slots[i].wg[k].as_ref().unwrap()[idx]
            }
        }
    }

    /// `getHeightmapPos`.
    pub fn heightmap_pos(&mut self, ty: Heightmap, p: Pos) -> Pos {
        Pos::new(p.x, self.height(ty, p.x, p.z), p.z)
    }

    /// `getBiome(pos)`: the jittered block-resolution biome.
    pub fn biome(&self, p: Pos) -> BiomeId {
        let (qx, qy, qz) = biome_manager::zoomed_quart(self.core.zoom_seed, p.x, p.y, p.z);
        let min_qy = self.core.min_y >> 2;
        let qy = qy.clamp(min_qy, min_qy + self.core.height / 4 - 1);
        if let Some(b) = self.biome_cache.borrow().get(&(qx, qy, qz)) {
            return *b;
        }
        let b = match self.slot_of(qx << 2, qz << 2) {
            Some(i) => self.slots[i]
                .chunk
                .biome((qx & 3) as usize, qy - min_qy, (qz & 3) as usize),
            None => self.core.noise_biome(qx, qy, qz),
        };
        self.biome_cache.borrow_mut().insert((qx, qy, qz), b);
        b
    }

    /// The distinct biomes stored in the 3×3 chunks, in id order.
    pub fn stored_biomes(&self) -> Vec<BiomeId> {
        let mut seen = vec![false; self.core.biomes.all().len()];
        for s in &self.slots {
            for b in s.chunk.biomes() {
                seen[*b as usize] = true;
            }
        }
        seen.iter()
            .enumerate()
            .filter(|(_, v)| **v)
            .map(|(i, _)| i as BiomeId)
            .collect()
    }

    /// `isEmptyBlock`.
    pub fn is_air(&self, p: Pos) -> bool {
        blockinfo::is_air(self.get(p))
    }
}

/// A `HashSet<BlockPos>` that remembers what Java's iteration order would be.
///
/// Tree decorators walk the tree's logs and leaves out of hash sets and draw
/// from the random per element, so the order of that walk is part of the
/// random stream. Java's `HashMap` iterates bucket by bucket, entries within
/// a bucket in insertion order; this reproduces that (without the treeified
/// buckets that only appear past eight collisions in one bucket).
#[derive(Debug, Default, Clone)]
pub struct JavaPosSet {
    order: Vec<Pos>,
    set: std::collections::HashSet<Pos>,
}

impl JavaPosSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }
    /// `add`.
    pub fn insert(&mut self, p: Pos) {
        if self.set.insert(p) {
            self.order.push(p);
        }
    }
    /// `contains`.
    pub fn contains(&self, p: &Pos) -> bool {
        self.set.contains(p)
    }
    /// `isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
    /// `size`.
    pub fn len(&self) -> usize {
        self.order.len()
    }
    /// Insertion order.
    pub fn inserted(&self) -> &[Pos] {
        &self.order
    }
    /// Java's iteration order.
    pub fn java_order(&self) -> Vec<Pos> {
        let n = self.order.len();
        let mut cap = 16usize;
        while n as f64 > cap as f64 * 0.75 {
            cap *= 2;
        }
        let mut keyed: Vec<(usize, usize, Pos)> = self
            .order
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let h = p.java_hash();
                let h = h ^ ((h as u32) >> 16) as i32;
                ((h as u32 as usize) & (cap - 1), i, *p)
            })
            .collect();
        keyed.sort_by_key(|(b, i, _)| (*b, *i));
        keyed.into_iter().map(|(_, _, p)| p).collect()
    }
}
