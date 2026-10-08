//! Light for one chunk column: building the masks from blocks, and holding the
//! result in a form the network layer can send cheaply.
//!
//! # Why a whole column and not a sub-chunk
//!
//! A sub-chunk's masks fit in registers, which is the attraction, but sky light
//! is dominated by *vertical* propagation and a 16-cell-tall unit would need
//! the frontier exchanged at every section boundary. Taking the column as the
//! unit removes that problem entirely — daylight falls from the top of the
//! world to the floor in one sweep — and leaves only the horizontal boundaries
//! between columns, which is the smaller half of the problem and is not solved
//! here either way.
//!
//! # Uniform sections
//!
//! Light is the largest thing this server sends: a 1.21 column carries
//! `2 x sections x 2048` bytes of it against roughly 8 KB of blocks. Most
//! sections are entirely one value — 15 above the terrain, 0 far below it, and
//! 0 for block light nearly everywhere — so they are kept as a single byte and
//! the protocol's "empty" bitset elides the all-zero ones from the wire
//! completely.

use super::compute::{self, Levels, Source};
use super::mask::{Mask, DIM};
use crate::block::BlockProperties;
use crate::BlockStateId;

/// Cells in one section.
pub const SECTION_VOLUME: usize = DIM * DIM * DIM;
/// Bytes a packed light section occupies on the wire: two levels per byte.
pub const PACKED_BYTES: usize = SECTION_VOLUME / 2;

/// One section's light, kept small when it can be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightSection {
    /// Every cell holds this level. One byte instead of 2048.
    Uniform(u8),
    /// Level per cell, in linear order.
    Dense(Box<[u8; SECTION_VOLUME]>),
}

impl LightSection {
    /// The level at a local coordinate.
    pub fn get(&self, x: usize, y: usize, z: usize) -> u8 {
        match self {
            LightSection::Uniform(v) => *v,
            LightSection::Dense(d) => d[Mask::index(x, y, z)],
        }
    }

    /// Whether this section is entirely dark — the case the protocol can send
    /// as a single bit.
    pub fn is_dark(&self) -> bool {
        matches!(self, LightSection::Uniform(0))
    }

    /// The section packed two levels to a byte, low nibble first, as the wire
    /// format wants.
    pub fn packed(&self) -> Vec<u8> {
        let mut out = vec![0u8; PACKED_BYTES];
        match self {
            LightSection::Uniform(v) => {
                let b = (v & 0x0F) | (v << 4);
                out.fill(b);
            }
            LightSection::Dense(d) => {
                for (i, pair) in d.chunks_exact(2).enumerate() {
                    out[i] = (pair[0] & 0x0F) | (pair[1] << 4);
                }
            }
        }
        out
    }
}

/// Sky and block light for a column, one entry per section, bottom first.
#[derive(Debug, Clone)]
pub struct ColumnLight {
    pub sky: Vec<LightSection>,
    pub block: Vec<LightSection>,
}

/// What the light needs to know about a column's blocks.
pub trait ColumnBlocks {
    /// Sections in this column.
    fn sections(&self) -> usize;
    /// The block at a column-local coordinate, `y` measured from the bottom of
    /// the column.
    fn block_at(&self, x: usize, y: usize, z: usize) -> BlockStateId;
    /// Properties of a block.
    fn props(&self, id: BlockStateId) -> BlockProperties;
}

/// Compute a column's light from its blocks.
pub fn compute_column<B: ColumnBlocks>(blocks: &B) -> ColumnLight {
    let sections = blocks.sections();
    let h = sections * DIM;

    let mut opaque = Mask::zeroed(h);
    let mut dim = Mask::zeroed(h);
    let mut sources: Vec<Source> = Vec::new();
    for y in 0..h {
        for z in 0..DIM {
            for x in 0..DIM {
                let p = blocks.props(blocks.block_at(x, y, z));
                if p.light_opacity >= 15 {
                    opaque.set(x, y, z, true);
                } else if p.light_opacity > 0 {
                    dim.set(x, y, z, true);
                }
                if p.light_emission > 0 {
                    sources.push(Source {
                        x,
                        y,
                        z,
                        level: p.light_emission.min(15),
                    });
                }
            }
        }
    }

    let sky = compute::sky_light(&opaque, &dim);
    let block = compute::block_light(&opaque, &sources);
    ColumnLight {
        sky: split(&sky, sections),
        block: split(&block, sections),
    }
}

/// Cut a column's levels into sections, collapsing the uniform ones.
fn split(levels: &Levels, sections: usize) -> Vec<LightSection> {
    (0..sections)
        .map(|s| {
            let base = s * DIM;
            let mut data = Box::new([0u8; SECTION_VOLUME]);
            let mut uniform = Some(levels.get(0, base, 0));
            for y in 0..DIM {
                for z in 0..DIM {
                    for x in 0..DIM {
                        let v = levels.get(x, base + y, z);
                        data[Mask::index(x, y, z)] = v;
                        if uniform != Some(v) {
                            uniform = None;
                        }
                    }
                }
            }
            match uniform {
                Some(v) => LightSection::Uniform(v),
                None => LightSection::Dense(data),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A column that is air above `floor` and stone at or below it, with an
    /// optional glowstone.
    struct TestColumn {
        sections: usize,
        floor: usize,
        lamp: Option<(usize, usize, usize)>,
    }

    impl ColumnBlocks for TestColumn {
        fn sections(&self) -> usize {
            self.sections
        }
        fn block_at(&self, x: usize, y: usize, z: usize) -> BlockStateId {
            if self.lamp == Some((x, y, z)) {
                return crate::registry::blocks::default_state("minecraft:glowstone").unwrap();
            }
            if y <= self.floor {
                crate::registry::ids::STONE
            } else {
                crate::registry::ids::AIR
            }
        }
        fn props(&self, id: BlockStateId) -> BlockProperties {
            crate::registry::blocks::props_of_state(id).unwrap_or(BlockProperties::AIR)
        }
    }

    #[test]
    fn a_section_of_one_value_costs_one_byte_not_two_thousand() {
        let u = LightSection::Uniform(15);
        assert_eq!(u.get(3, 4, 5), 15);
        assert_eq!(u.packed().len(), PACKED_BYTES);
        assert!(u.packed().iter().all(|b| *b == 0xFF));
        assert!(!u.is_dark());
        assert!(LightSection::Uniform(0).is_dark());
    }

    #[test]
    fn packing_puts_the_first_cell_in_the_low_nibble() {
        // Get this backwards and every light level is swapped with its
        // neighbour — which reads as noise, not as an off-by-one.
        let mut d = Box::new([0u8; SECTION_VOLUME]);
        d[0] = 1;
        d[1] = 2;
        let packed = LightSection::Dense(d).packed();
        assert_eq!(packed[0], 0x21, "low nibble is cell 0");
    }

    #[test]
    fn an_open_column_is_all_sky_and_no_block_light() {
        // And both halves collapse to one byte a section, which is the whole
        // bandwidth argument.
        // The floor is at y=20, inside section 1, so section 0 is entirely
        // below it and section 7 entirely above.
        let c = compute_column(&TestColumn {
            sections: 8,
            floor: 20,
            lamp: None,
        });
        assert_eq!(c.sky.len(), 8);
        assert!(
            c.block.iter().all(|s| *s == LightSection::Uniform(0)),
            "no lamps means no block light anywhere"
        );
        assert_eq!(c.sky[7], LightSection::Uniform(15), "open sky");
        assert_eq!(c.sky[0], LightSection::Uniform(0), "buried");
    }

    #[test]
    fn the_section_the_terrain_crosses_is_the_only_dense_one() {
        // The claim the design rests on: only the band the surface passes
        // through needs 2048 bytes.
        let c = compute_column(&TestColumn {
            sections: 8,
            floor: 20, // inside section 1
            lamp: None,
        });
        let dense = c
            .sky
            .iter()
            .filter(|s| matches!(s, LightSection::Dense(_)))
            .count();
        assert_eq!(dense, 1, "{:?}", c.sky);
    }

    #[test]
    fn a_lamp_lights_its_own_section_and_leaves_the_rest_dark() {
        // The lamp hangs in open air at y=20; the floor is far below it, so
        // it is not walled in.
        let c = compute_column(&TestColumn {
            sections: 8,
            floor: 4,
            lamp: Some((8, 20, 8)),
        });
        assert_eq!(c.block[1].get(8, 4, 8), 15, "the lamp itself");
        assert_eq!(c.block[1].get(9, 4, 8), 14);
        // Far away, nothing — and it costs one byte to say so.
        assert!(c.block[5].is_dark());
        assert!(c.block[7].is_dark(), "15 levels does not reach section 7");
    }

    #[test]
    fn light_crosses_a_section_boundary_without_a_seam() {
        // The reason the column is the unit: a per-section computation would
        // need the frontier handing over at every boundary, and forgetting to
        // shows up as a flat line of darkness at a multiple of sixteen.
        let c = compute_column(&TestColumn {
            sections: 4,
            floor: 0,
            lamp: Some((8, 15, 8)), // the top cell of section 0
        });
        assert_eq!(c.block[0].get(8, 15, 8), 15);
        assert_eq!(c.block[1].get(8, 0, 8), 14, "one cell up, next section");
        assert_eq!(c.block[1].get(8, 1, 8), 13);
    }
}
