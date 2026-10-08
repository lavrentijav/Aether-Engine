//! Measure what the terrain generator actually produces.
//!
//! Terrain quality is easy to wreck by tuning constants against a screenshot,
//! so every claim about this generator should come from here instead: land
//! versus water, how high and how varied the surface is, how steep it gets
//! between neighbouring columns, and whether the 3D density field is really
//! carving caves rather than degenerating into a smooth heightmap.
//!
//! Chunks are sampled **scattered over a wide area**, not as one contiguous
//! block. The control fields that decide ocean-versus-land have a wavelength of
//! several hundred blocks, so a compact sample sits inside a single continent
//! and reports that continent's verdict as if it were the world's — which is
//! how the same generator can measure 3% land on one seed and 79% on another.
//!
//! ```text
//! cargo run -p aether-worldgen --example terrain_stats -- [seed] [radius] [stride]
//! ```

#![allow(clippy::type_complexity)]
#![allow(clippy::needless_range_loop)]

use aether_world::registry::ids;
use aether_world::BlockStateId;
use aether_worldgen::{ChunkGenerator, GeneratedColumn, NoiseGenerator};

/// One column's vertical profile, flattened out of the sub-chunk sections.
struct Column {
    blocks: Vec<BlockStateId>,
}

impl Column {
    fn from(col: &GeneratedColumn, lx: usize, lz: usize, height: i32) -> Self {
        let mut blocks = vec![BlockStateId::AIR; height as usize];
        for (cy, sc) in &col.sections {
            for ly in 0..16 {
                let world_y = *cy as i32 * 16 + ly as i32;
                if (0..height).contains(&world_y) {
                    blocks[world_y as usize] = sc.get(lx, ly, lz);
                }
            }
        }
        Self { blocks }
    }

    /// Topmost block that is neither air nor water — the ground surface.
    fn surface(&self) -> Option<i32> {
        (0..self.blocks.len()).rev().find_map(|y| {
            let b = self.blocks[y];
            (b != BlockStateId::AIR && b != ids::WATER).then_some(y as i32)
        })
    }

    /// Whether the column's topmost block is water: an ocean/lake column.
    fn is_water(&self) -> bool {
        (0..self.blocks.len())
            .rev()
            .find(|y| self.blocks[*y] != BlockStateId::AIR)
            .is_some_and(|y| self.blocks[y] == ids::WATER)
    }

    /// An enclosed air pocket with solid rock both above and below is a cave;
    /// it cannot exist if the density field has collapsed into a heightmap.
    fn has_cave(&self) -> bool {
        let Some(surface) = self.surface() else {
            return false;
        };
        let mut seen_solid_above = false;
        for y in (1..surface).rev() {
            let solid = self.blocks[y as usize] != BlockStateId::AIR
                && self.blocks[y as usize] != ids::WATER;
            if solid {
                seen_solid_above = true;
            } else if seen_solid_above && self.blocks[(y - 1) as usize] != BlockStateId::AIR {
                return true;
            }
        }
        false
    }
}

fn percentile(sorted: &[i32], p: f64) -> i32 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let seed: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(42);
    // Defaults span ±160 chunks (±2560 blocks), far wider than the control
    // fields' wavelength, taking every 16th chunk so the cost stays bounded.
    let radius: i32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(160);
    let stride: i32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(16);

    let gen = NoiseGenerator::new(seed);
    let sea = gen.sea_level();
    let height = gen.world_height();

    let mut surfaces: Vec<i32> = Vec::new();
    let mut water_columns = 0usize;
    let mut cave_columns = 0usize;
    let mut total = 0usize;
    // Height difference between horizontally adjacent columns, as a proxy for
    // "rolling hills" versus "sheer walls".
    let mut steps: Vec<i32> = Vec::new();

    for cz in (-radius..=radius).step_by(stride as usize) {
        for cx in (-radius..=radius).step_by(stride as usize) {
            let col = gen.generate_column(cx, cz);
            let mut grid = [[0i32; 16]; 16];
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let c = Column::from(&col, lx, lz, height);
                    total += 1;
                    if c.is_water() {
                        water_columns += 1;
                    }
                    if c.has_cave() {
                        cave_columns += 1;
                    }
                    let s = c.surface().unwrap_or(0);
                    grid[lx][lz] = s;
                    surfaces.push(s);
                }
            }
            for lz in 0..16usize {
                for lx in 1..16usize {
                    steps.push((grid[lx][lz] - grid[lx - 1][lz]).abs());
                }
            }
        }
    }

    surfaces.sort_unstable();
    steps.sort_unstable();
    let land = total - water_columns;
    let pct = |n: usize| 100.0 * n as f64 / total as f64;

    println!(
        "seed {seed}, {} columns, sea level {sea}, height {height}",
        total
    );
    println!(
        "land   : {:.1}%  ({land} columns)   water: {:.1}%",
        pct(land),
        pct(water_columns)
    );
    println!(
        "surface: min {}  p10 {}  median {}  p90 {}  max {}",
        surfaces[0],
        percentile(&surfaces, 0.10),
        percentile(&surfaces, 0.50),
        percentile(&surfaces, 0.90),
        surfaces[surfaces.len() - 1]
    );
    println!(
        "step   : median {}  p90 {}  p99 {}  max {}",
        percentile(&steps, 0.50),
        percentile(&steps, 0.90),
        percentile(&steps, 0.99),
        steps[steps.len() - 1]
    );
    println!("caves  : {:.1}% of columns", pct(cave_columns));
}
