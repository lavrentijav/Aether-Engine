//! A one-bit-per-cell mask over a 16 x 16 x N column, and the six-way dilation
//! the light propagation is built from.
//!
//! # Why linear order here, when everything else is Morton
//!
//! The block data is Morton-ordered, which is right for neighbourhood access.
//! It is wrong for this: `+1 in x` is not a shift in Morton order, it is a
//! magic-bits increment per index, so a frontier cannot be advanced a whole
//! word at a time.
//!
//! In **linear** order — `x + 16*z + 256*y` — the six neighbour offsets are:
//!
//! | direction | shift |
//! |---|---|
//! | ±x | 1 bit, blanked at the row edge |
//! | ±z | 16 bits, blanked at the layer edge |
//! | ±y | 256 bits, which is four whole words |
//!
//! That is the whole reason this type exists. The transpose from Morton is
//! paid once, on the way in, and the client's light section is *also* linear —
//! so computing here removes a transpose at send time rather than adding one.

/// Cells along each horizontal edge.
pub const DIM: usize = 16;
/// Cells in one horizontal layer.
pub const LAYER: usize = DIM * DIM;
/// Bits in one `u64`.
const BITS: usize = 64;
/// Words in one horizontal layer: 256 bits.
const LAYER_WORDS: usize = LAYER / BITS;

/// Bits whose `x` is 15 — the cells a `+x` step must not run past.
///
/// One word holds four rows of sixteen, so the pattern repeats every 16 bits.
const X_MAX: u64 = 0x8000_8000_8000_8000;
/// Bits whose `x` is 0.
const X_MIN: u64 = 0x0001_0001_0001_0001;

/// A bit per cell of a 16 x 16 x `height` column, in linear order.
#[derive(Clone, PartialEq, Eq)]
pub struct Mask {
    words: Vec<u64>,
    height: usize,
}

impl std::fmt::Debug for Mask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mask({DIM}x{DIM}x{}, {} set)", self.height, self.count())
    }
}

impl Mask {
    /// An all-zero mask `height` cells tall.
    pub fn zeroed(height: usize) -> Self {
        Self {
            words: vec![0; height * LAYER_WORDS],
            height,
        }
    }

    /// Height in cells.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Linear index of `(x, y, z)`.
    #[inline]
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        x + DIM * z + LAYER * y
    }

    /// Whether `(x, y, z)` is set.
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> bool {
        let i = Self::index(x, y, z);
        self.words[i / BITS] >> (i % BITS) & 1 != 0
    }

    /// Set or clear `(x, y, z)`.
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, on: bool) {
        let i = Self::index(x, y, z);
        let (w, b) = (i / BITS, i % BITS);
        if on {
            self.words[w] |= 1 << b;
        } else {
            self.words[w] &= !(1 << b);
        }
    }

    /// How many cells are set.
    pub fn count(&self) -> u32 {
        self.words.iter().map(|w| w.count_ones()).sum()
    }

    /// Whether no cell is set.
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|w| *w == 0)
    }

    /// `self &= !other`.
    pub fn and_not(&mut self, other: &Mask) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a &= !*b;
        }
    }

    /// `self &= other`.
    pub fn and(&mut self, other: &Mask) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a &= *b;
        }
    }

    /// `self |= other`.
    pub fn or(&mut self, other: &Mask) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a |= *b;
        }
    }

    /// The raw words, for callers that vectorise.
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// Every cell that is a face-neighbour of a set cell — the set dilated one
    /// step in all six directions, the original included.
    ///
    /// The edge blanking is the part that is easy to get wrong and impossible
    /// to see: without it a `+x` step at `x = 15` wraps into `x = 0` of the
    /// next row, and light leaks across the column in a way that looks like a
    /// plausible cave.
    pub fn dilate6(&self) -> Mask {
        let mut out = self.clone();
        let n = self.words.len();

        // ±x: one bit, with the wrapping edge blanked first.
        for w in 0..n {
            let plus = (self.words[w] & !X_MAX) << 1;
            let minus = (self.words[w] & !X_MIN) >> 1;
            out.words[w] |= plus | minus;
        }

        // ±z: sixteen bits, staying inside the same y layer. A layer is four
        // words, so the shift crosses word boundaries and carries.
        for layer in 0..self.height {
            let base = layer * LAYER_WORDS;
            let src: [u64; LAYER_WORDS] = [
                self.words[base],
                self.words[base + 1],
                self.words[base + 2],
                self.words[base + 3],
            ];
            for w in 0..LAYER_WORDS {
                let up = (src[w] << 16) | if w > 0 { src[w - 1] >> 48 } else { 0 };
                let down = (src[w] >> 16)
                    | if w + 1 < LAYER_WORDS {
                        src[w + 1] << 48
                    } else {
                        0
                    };
                out.words[base + w] |= up | down;
            }
        }

        // ±y: a whole layer, which is a word move and no bit shifting at all.
        for layer in 0..self.height {
            let base = layer * LAYER_WORDS;
            for w in 0..LAYER_WORDS {
                if layer > 0 {
                    out.words[base + w] |= self.words[base - LAYER_WORDS + w];
                }
                if layer + 1 < self.height {
                    out.words[base + w] |= self.words[base + LAYER_WORDS + w];
                }
            }
        }
        out
    }

    /// Every cell directly below a set cell — one step down only.
    ///
    /// Sky light's special case: it does not attenuate going straight down, so
    /// the downward sweep is separate from the six-way dilation.
    pub fn shift_down(&self) -> Mask {
        let mut out = Mask::zeroed(self.height);
        for layer in 1..self.height {
            let (dst, src) = ((layer - 1) * LAYER_WORDS, layer * LAYER_WORDS);
            for w in 0..LAYER_WORDS {
                out.words[dst + w] = self.words[src + w];
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(h: usize, x: usize, y: usize, z: usize) -> Mask {
        let mut m = Mask::zeroed(h);
        m.set(x, y, z, true);
        m
    }

    /// The neighbours of a cell, worked out from the *definition* of a face
    /// neighbour rather than from the shift arithmetic under test.
    fn neighbours(x: usize, y: usize, z: usize, h: usize) -> Vec<(usize, usize, usize)> {
        let mut out = vec![(x, y, z)];
        let (xi, yi, zi) = (x as i32, y as i32, z as i32);
        for (dx, dy, dz) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let (nx, ny, nz) = (xi + dx, yi + dy, zi + dz);
            if (0..DIM as i32).contains(&nx)
                && (0..h as i32).contains(&ny)
                && (0..DIM as i32).contains(&nz)
            {
                out.push((nx as usize, ny as usize, nz as usize));
            }
        }
        out
    }

    #[test]
    fn index_is_the_linear_order_the_client_uses() {
        // x fastest, then z, then y — the order a light section arrives in.
        assert_eq!(Mask::index(0, 0, 0), 0);
        assert_eq!(Mask::index(1, 0, 0), 1);
        assert_eq!(Mask::index(0, 0, 1), 16);
        assert_eq!(Mask::index(0, 1, 0), 256);
        assert_eq!(Mask::index(15, 15, 15), 4095);
    }

    #[test]
    fn a_bit_reads_back_where_it_was_written() {
        let mut m = Mask::zeroed(32);
        for (x, y, z) in [(0, 0, 0), (15, 31, 15), (7, 16, 3), (1, 1, 1)] {
            m.set(x, y, z, true);
            assert!(m.get(x, y, z), "{x},{y},{z}");
        }
        assert_eq!(m.count(), 4);
        m.set(7, 16, 3, false);
        assert!(!m.get(7, 16, 3));
        assert_eq!(m.count(), 3);
    }

    #[test]
    fn dilation_reaches_exactly_the_six_face_neighbours() {
        // Checked against the definition, at the middle and at every kind of
        // edge and corner.
        let h = 8;
        for (x, y, z) in [(5, 4, 5), (0, 0, 0), (15, 7, 15), (0, 4, 15), (15, 0, 0)] {
            let got = one(h, x, y, z).dilate6();
            let want = neighbours(x, y, z, h);
            assert_eq!(got.count() as usize, want.len(), "at {x},{y},{z}");
            for (nx, ny, nz) in want {
                assert!(
                    got.get(nx, ny, nz),
                    "{x},{y},{z} should reach {nx},{ny},{nz}"
                );
            }
        }
    }

    #[test]
    fn a_step_in_x_never_wraps_into_the_next_row() {
        // The bug this exists to catch: without blanking, `+x` at x=15 lands
        // at x=0 of the row behind, and light leaks across the column in a way
        // that looks like a plausible cave.
        let m = one(4, 15, 1, 5).dilate6();
        assert!(!m.get(0, 1, 5), "wrapped forwards within the row");
        assert!(!m.get(0, 1, 6), "wrapped into the next row");
        assert!(m.get(14, 1, 5), "but it must still step backwards");

        let m = one(4, 0, 1, 5).dilate6();
        assert!(!m.get(15, 1, 5));
        assert!(!m.get(15, 1, 4));
        assert!(m.get(1, 1, 5));
    }

    #[test]
    fn a_step_in_z_never_wraps_into_the_layer_above_or_below() {
        let m = one(4, 5, 1, 15).dilate6();
        assert!(!m.get(5, 2, 0), "wrapped into the next layer");
        assert!(!m.get(5, 1, 0));
        assert!(m.get(5, 1, 14));

        let m = one(4, 5, 1, 0).dilate6();
        assert!(!m.get(5, 0, 15));
        assert!(m.get(5, 1, 1));
    }

    #[test]
    fn a_step_in_y_stops_at_the_top_and_bottom() {
        let h = 6;
        let top = one(h, 5, h - 1, 5).dilate6();
        assert_eq!(top.count() as usize, neighbours(5, h - 1, 5, h).len());
        let bottom = one(h, 5, 0, 5).dilate6();
        assert_eq!(bottom.count() as usize, neighbours(5, 0, 5, h).len());
    }

    #[test]
    fn dilating_a_full_mask_changes_nothing() {
        let mut full = Mask::zeroed(4);
        for y in 0..4 {
            for z in 0..DIM {
                for x in 0..DIM {
                    full.set(x, y, z, true);
                }
            }
        }
        assert_eq!(full.dilate6(), full);
    }

    #[test]
    fn dilating_nothing_gives_nothing() {
        assert!(Mask::zeroed(8).dilate6().is_empty());
    }

    #[test]
    fn dilation_after_n_steps_is_a_diamond_of_radius_n() {
        // Manhattan distance: the shape a uniform-cost flood fill makes, and
        // the property the whole level-by-level scheme rests on.
        let h = 16;
        let (cx, cy, cz) = (8, 8, 8);
        let mut m = one(h, cx, cy, cz);
        for step in 1..=4usize {
            m = m.dilate6();
            for y in 0..h {
                for z in 0..DIM {
                    for x in 0..DIM {
                        let d = x.abs_diff(cx) + y.abs_diff(cy) + z.abs_diff(cz);
                        assert_eq!(
                            m.get(x, y, z),
                            d <= step,
                            "step {step} at {x},{y},{z} (distance {d})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn shifting_down_moves_one_layer_and_drops_the_bottom() {
        let m = one(4, 3, 2, 7).shift_down();
        assert!(m.get(3, 1, 7));
        assert_eq!(m.count(), 1);
        // Nothing survives falling out of the bottom.
        assert!(one(4, 3, 0, 7).shift_down().is_empty());
    }

    #[test]
    fn and_not_and_or_are_elementwise() {
        let a = one(4, 1, 1, 1);
        let b = one(4, 2, 1, 1);
        let mut both = a.clone();
        both.or(&b);
        assert_eq!(both.count(), 2);
        both.and_not(&a);
        assert_eq!(both.count(), 1);
        assert!(both.get(2, 1, 1));
    }
}
