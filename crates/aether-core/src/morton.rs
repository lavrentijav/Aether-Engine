//! Morton (Z-order) indexing for the 16×16×16 sub-chunk.
//!
//! Each coordinate is 4 bits (`0..=15`); the interleaved index is 12 bits
//! (`0..=4095`). Z-order keeps neighbours in space close in memory, which is
//! what lets the [`crate::simd`] mask operations stream whole cache lines.
//!
//! The interleave order is `x` in bit 0, `y` in bit 1, `z` in bit 2, repeating
//! — i.e. `index = Σ (xᵢ<<(3i)) | (yᵢ<<(3i+1)) | (zᵢ<<(3i+2))`.

/// Spread the low 4 bits of `v` so bit `i` lands at position `3·i`.
#[inline]
const fn part_1_by_2(v: u16) -> u16 {
    let mut x = v & 0x000f; // keep 4 bits
    x = (x | (x << 8)) & 0x100f;
    x = (x | (x << 4)) & 0x10c3;
    x = (x | (x << 2)) & 0x1249;
    x
}

/// Inverse of [`part_1_by_2`]: gather bits at positions `3·i` back into the low 4.
#[inline]
const fn compact_1_by_2(v: u16) -> u16 {
    let mut x = v & 0x1249;
    x = (x | (x >> 2)) & 0x10c3;
    x = (x | (x >> 4)) & 0x100f;
    x = (x | (x >> 8)) & 0x000f;
    x
}

/// Encode a `(x, y, z)` position inside a sub-chunk (each `0..=15`) into a
/// 12-bit Morton index (`0..=4095`).
///
/// The three low nibbles of each coordinate are used; higher bits are ignored,
/// so callers are free to pass already-masked values.
#[inline]
pub const fn morton_encode_16(x: u8, y: u8, z: u8) -> u16 {
    part_1_by_2(x as u16) | (part_1_by_2(y as u16) << 1) | (part_1_by_2(z as u16) << 2)
}

/// Decode a 12-bit Morton index back into its `(x, y, z)` position.
#[inline]
pub const fn morton_decode_16(index: u16) -> (u8, u8, u8) {
    let x = compact_1_by_2(index) as u8;
    let y = compact_1_by_2(index >> 1) as u8;
    let z = compact_1_by_2(index >> 2) as u8;
    (x, y, z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_position() {
        for z in 0..16u8 {
            for y in 0..16u8 {
                for x in 0..16u8 {
                    let idx = morton_encode_16(x, y, z);
                    assert!(idx < 4096, "index {idx} out of range for ({x},{y},{z})");
                    assert_eq!(morton_decode_16(idx), (x, y, z));
                }
            }
        }
    }

    #[test]
    fn indices_are_a_bijection_over_0_4095() {
        let mut seen = [false; 4096];
        for z in 0..16u8 {
            for y in 0..16u8 {
                for x in 0..16u8 {
                    let idx = morton_encode_16(x, y, z) as usize;
                    assert!(!seen[idx], "duplicate morton index {idx}");
                    seen[idx] = true;
                }
            }
        }
        assert!(seen.iter().all(|&s| s), "morton map did not cover 0..4096");
    }

    #[test]
    fn known_values() {
        assert_eq!(morton_encode_16(0, 0, 0), 0);
        assert_eq!(morton_encode_16(1, 0, 0), 0b001);
        assert_eq!(morton_encode_16(0, 1, 0), 0b010);
        assert_eq!(morton_encode_16(0, 0, 1), 0b100);
        assert_eq!(morton_encode_16(15, 15, 15), 4095);
    }
}
