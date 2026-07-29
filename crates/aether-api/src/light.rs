//! Real lighting over a [`World`](crate::World) chunk column.
//!
//! [`World::light_column`](crate::World::light_column) snapshots the opacity and
//! emission of one chunk column (its full vertical band) and runs the
//! [`aether_world::compute_light`] flood-fill over it, returning a
//! [`ColumnLight`] that answers block / sky light in **world** coordinates.
//!
//! This replaces the [`FullBright`](aether_world::FullBright) fallback with
//! computed light. Because it works one column at a time, horizontal light
//! bleed across chunk borders is not modelled — the registered Phase 1
//! deviation, closed in Phase 2 by the cross-chunk safe-point merge.

use aether_world::{compute_light, LightGrid, LightMedium, LightView, MAX_LIGHT};

/// Opacity + emission snapshot of one chunk column, sized `16 × height × 16`
/// with local `y = 0` mapping to world `base_y`.
pub(crate) struct ColumnMedium {
    width: usize,
    height: usize,
    depth: usize,
    opaque: Vec<bool>,
    emission: Vec<u8>,
}

impl ColumnMedium {
    pub(crate) fn new(width: usize, height: usize, depth: usize) -> Self {
        let n = width * height * depth;
        Self {
            width,
            height,
            depth,
            opaque: vec![false; n],
            emission: vec![0; n],
        }
    }

    #[inline]
    pub(crate) fn idx(&self, x: usize, y: usize, z: usize) -> usize {
        (y * self.depth + z) * self.width + x
    }

    #[inline]
    pub(crate) fn set(&mut self, x: usize, y: usize, z: usize, opaque: bool, emission: u8) {
        let i = self.idx(x, y, z);
        self.opaque[i] = opaque;
        self.emission[i] = emission;
    }
}

impl LightMedium for ColumnMedium {
    fn dims(&self) -> (usize, usize, usize) {
        (self.width, self.height, self.depth)
    }
    #[inline]
    fn opaque(&self, x: usize, y: usize, z: usize) -> bool {
        self.opaque[self.idx(x, y, z)]
    }
    #[inline]
    fn emission(&self, x: usize, y: usize, z: usize) -> u8 {
        self.emission[self.idx(x, y, z)]
    }
}

/// Computed light for one chunk column, queryable in world coordinates.
///
/// Coordinates outside this column's `16 × 16` footprint or vertical band read
/// as fully dark for block light; sky light reads as [`MAX_LIGHT`] above the
/// band (open sky) and dark below it.
#[derive(Clone)]
pub struct ColumnLight {
    cx: i32,
    cz: i32,
    base_y: i32,
    grid: LightGrid,
}

impl ColumnLight {
    pub(crate) fn new(cx: i32, cz: i32, base_y: i32, medium: &ColumnMedium) -> Self {
        Self {
            cx,
            cz,
            base_y,
            grid: compute_light(medium),
        }
    }

    /// The chunk column `(cx, cz)` this light covers.
    pub fn column(&self) -> (i32, i32) {
        (self.cx, self.cz)
    }

    /// World `y` of the bottom of the lit band.
    pub fn base_y(&self) -> i32 {
        self.base_y
    }

    /// Map world coords into local grid coords if they fall inside this column.
    #[inline]
    fn local(&self, x: i32, y: i32, z: i32) -> Option<(usize, usize, usize)> {
        let (w, h, d) = self.grid.dims();
        let lx = x - self.cx * 16;
        let lz = z - self.cz * 16;
        let ly = y - self.base_y;
        if lx < 0 || lz < 0 || ly < 0 {
            return None;
        }
        let (lx, ly, lz) = (lx as usize, ly as usize, lz as usize);
        if lx >= w || ly >= h || lz >= d {
            return None;
        }
        Some((lx, ly, lz))
    }

    /// Is world `y` above the top of the lit band (open sky)?
    #[inline]
    fn above_band(&self, y: i32) -> bool {
        let (_w, h, _d) = self.grid.dims();
        y >= self.base_y + h as i32
    }
}

impl LightView for ColumnLight {
    fn block_light(&self, x: i32, y: i32, z: i32) -> u8 {
        match self.local(x, y, z) {
            Some((lx, ly, lz)) => self.grid.block_light(lx, ly, lz),
            None => 0,
        }
    }

    fn sky_light(&self, x: i32, y: i32, z: i32) -> u8 {
        match self.local(x, y, z) {
            Some((lx, ly, lz)) => self.grid.sky_light(lx, ly, lz),
            // Open sky above the band; darkness below/outside it.
            None if self.above_band(y) => MAX_LIGHT,
            None => 0,
        }
    }
}
