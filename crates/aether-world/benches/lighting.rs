//! Flood-fill lighting throughput: block + sky light over a column-sized
//! volume with terrain and scattered emitters.

use aether_world::{compute_light, LightMedium};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// A synthetic column: solid ground up to `surface`, open air above, with a
/// sparse 3D grid of emitters buried in the ground to exercise the block-light
/// BFS alongside the sky-light pass.
struct Column {
    w: usize,
    h: usize,
    d: usize,
    surface: usize,
}

impl Column {
    fn new(w: usize, h: usize, d: usize, surface: usize) -> Self {
        Self { w, h, d, surface }
    }
}

impl LightMedium for Column {
    fn dims(&self) -> (usize, usize, usize) {
        (self.w, self.h, self.d)
    }
    fn opaque(&self, _x: usize, y: usize, _z: usize) -> bool {
        // Solid terrain below the surface, open air above.
        y < self.surface
    }
    fn emission(&self, x: usize, y: usize, z: usize) -> u8 {
        // Emitters on a coarse lattice inside the ground.
        if y < self.surface && x % 8 == 4 && z % 8 == 4 && y % 8 == 4 {
            14
        } else {
            0
        }
    }
}

/// Open air with a single central emitter, so the BFS touches every cell.
struct CentralEmitter {
    w: usize,
    h: usize,
    d: usize,
}
impl CentralEmitter {
    fn new(w: usize, h: usize, d: usize) -> Self {
        Self { w, h, d }
    }
}
impl LightMedium for CentralEmitter {
    fn dims(&self) -> (usize, usize, usize) {
        (self.w, self.h, self.d)
    }
    fn opaque(&self, _x: usize, _y: usize, _z: usize) -> bool {
        false
    }
    fn emission(&self, x: usize, y: usize, z: usize) -> u8 {
        if x == self.w / 2 && y == self.h / 2 && z == self.d / 2 {
            15
        } else {
            0
        }
    }
}

fn bench_lighting(c: &mut Criterion) {
    // A full 16-wide chunk column, 256 tall, terrain below y=64.
    let column = Column::new(16, 256, 16, 64);
    c.bench_function("light_column_16x256x16", |b| {
        b.iter(|| black_box(compute_light(black_box(&column))))
    });

    // A dense block-light spread across a 32³ open cube.
    let cube = CentralEmitter::new(32, 32, 32);
    c.bench_function("block_light_spread_32cubed", |b| {
        b.iter(|| black_box(compute_light(black_box(&cube))))
    });
}

criterion_group!(benches, bench_lighting);
criterion_main!(benches);
