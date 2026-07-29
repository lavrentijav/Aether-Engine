//! Flow-field build throughput: one Dijkstra integration + direction pass over
//! a large grid with scattered obstacles (the cost a crowd amortises once).

use aether_ai::{FlowField, NavGrid};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn grid_with_obstacles(w: usize, d: usize) -> NavGrid {
    let mut g = NavGrid::new(w, d);
    // A sparse lattice of pillars to make the search do real work.
    for z in (4..d).step_by(7) {
        for x in (4..w).step_by(5) {
            g.block(x, z);
        }
    }
    g
}

fn bench_flowfield(c: &mut Criterion) {
    let grid = grid_with_obstacles(256, 256);
    c.bench_function("flowfield_256x256", |b| {
        b.iter(|| black_box(FlowField::toward(black_box(&grid), (128, 128))))
    });
}

criterion_group!(benches, bench_flowfield);
criterion_main!(benches);
