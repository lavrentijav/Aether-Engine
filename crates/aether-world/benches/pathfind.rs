//! Throughput of the mob A* search over a pillar field — the shape a zombie
//! sees in a cave or a forest. `expanded` is the nodes the search burned, so
//! the benchmark shows both time and work.

use std::collections::HashSet;

use aether_world::pathfind::{search, Nav};
use aether_world::registry::shapes::Box6;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

const FULL: [Box6; 1] = [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]];
const NONE: [Box6; 0] = [];

/// A floor with two-block-tall pillars on a regular grid, so the route winds
/// instead of running straight.
struct Pillars {
    solid: HashSet<[i32; 3]>,
}

impl Pillars {
    fn new(w: i32, d: i32) -> Self {
        let mut solid = HashSet::new();
        for x in 0..w {
            for z in 0..d {
                solid.insert([x, 0, z]);
            }
        }
        let mut x = 3;
        while x < w - 2 {
            let mut z = 3;
            while z < d - 2 {
                solid.insert([x, 1, z]);
                solid.insert([x, 2, z]);
                z += 4;
            }
            x += 4;
        }
        Self { solid }
    }
}

impl Nav for Pillars {
    fn boxes(&self, x: i32, y: i32, z: i32) -> &'static [Box6] {
        if self.solid.contains(&[x, y, z]) {
            &FULL
        } else {
            &NONE
        }
    }
    fn hazard(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

fn bench_pathfind(c: &mut Criterion) {
    let grid = Pillars::new(64, 64);
    let start = [1, 1, 1];
    let goal = [62, 1, 62];

    let mut group = c.benchmark_group("pathfind_search");
    group.throughput(Throughput::Elements(1));
    group.bench_function("64x64_pillars", |be| {
        be.iter(|| {
            let r = search(black_box(&grid), start, goal, 1.2, 100_000);
            black_box(r.expanded);
            black_box(r.reached)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_pathfind);
criterion_main!(benches);
