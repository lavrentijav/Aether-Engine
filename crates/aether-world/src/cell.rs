//! The AVX-Cell — the atomic unit of the memory model.
//!
//! A cell is a 4×4×2 = 32-block box stored as `[u16; 32]` = **64 bytes**,
//! aligned to a 64-byte boundary so it occupies exactly one CPU cache line and
//! one aligned AVX-512 load (or two AVX2 / four SSE loads).

/// 4×4×2 block of `BlockStateId` payloads, cache-line sized and aligned.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvxCell {
    blocks: [u16; 32],
}

impl AvxCell {
    /// Blocks along X within a cell.
    pub const DIM_X: usize = 4;
    /// Blocks along Y within a cell.
    pub const DIM_Y: usize = 2;
    /// Blocks along Z within a cell.
    pub const DIM_Z: usize = 4;
    /// Total blocks in a cell.
    pub const LEN: usize = 32;

    /// A cell full of the given raw payload.
    #[inline]
    pub const fn filled(value: u16) -> Self {
        Self {
            blocks: [value; 32],
        }
    }

    /// An all-zero (air) cell.
    #[inline]
    pub const fn empty() -> Self {
        Self::filled(0)
    }

    /// Local index of `(x, y, z)` within the cell (`x<4, y<2, z<4`).
    #[inline]
    pub const fn local_index(x: usize, y: usize, z: usize) -> usize {
        // Row-major within the cell; the sub-chunk applies Morton order at the
        // higher level, so cell-local order only needs to be consistent.
        (z * Self::DIM_Y + y) * Self::DIM_X + x
    }

    /// Read a block payload by local coordinate.
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> u16 {
        self.blocks[Self::local_index(x, y, z)]
    }

    /// Write a block payload by local coordinate.
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, value: u16) {
        self.blocks[Self::local_index(x, y, z)] = value;
    }

    /// The raw payload slice.
    #[inline]
    pub fn as_slice(&self) -> &[u16; 32] {
        &self.blocks
    }
}

impl Default for AvxCell {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_is_exactly_one_cache_line() {
        assert_eq!(std::mem::size_of::<AvxCell>(), 64);
        assert_eq!(std::mem::align_of::<AvxCell>(), 64);
    }

    #[test]
    fn local_index_is_a_bijection() {
        let mut seen = [false; AvxCell::LEN];
        for z in 0..AvxCell::DIM_Z {
            for y in 0..AvxCell::DIM_Y {
                for x in 0..AvxCell::DIM_X {
                    let i = AvxCell::local_index(x, y, z);
                    assert!(!seen[i]);
                    seen[i] = true;
                }
            }
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn get_set_round_trip() {
        let mut c = AvxCell::empty();
        c.set(3, 1, 2, 0xbeef);
        assert_eq!(c.get(3, 1, 2), 0xbeef);
        assert_eq!(c.get(0, 0, 0), 0);
    }
}
