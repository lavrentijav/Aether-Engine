//! Redstone solve throughput: compile a large grid of wire with scattered
//! sources/repeaters, then re-solve (the per-change tick cost).

use aether_redstone::{RedstoneGraph, RedstoneGraphBuilder};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// A `side x side` lattice of wires linked to their neighbours, with a source
/// every 32 cells and a repeater every 16 along rows to keep signal alive.
fn grid(side: usize) -> (RedstoneGraph, Vec<aether_redstone::NodeId>) {
    let mut b = RedstoneGraphBuilder::new();
    let mut ids = Vec::with_capacity(side * side);
    let mut sources = Vec::new();
    for i in 0..side * side {
        if i % 1024 == 0 {
            let s = b.add_source(15);
            sources.push(s);
            ids.push(s);
        } else if i % 16 == 0 {
            ids.push(b.add_repeater());
        } else {
            ids.push(b.add_wire());
        }
    }
    for z in 0..side {
        for x in 0..side {
            let here = ids[z * side + x];
            if x + 1 < side {
                b.link_both(here, ids[z * side + x + 1]);
            }
            if z + 1 < side {
                b.link_both(here, ids[(z + 1) * side + x]);
            }
        }
    }
    (b.build(), sources)
}

fn bench_redstone(c: &mut Criterion) {
    let (mut g, sources) = grid(100); // 10k nodes
    c.bench_function("redstone_solve_10k", |b| {
        b.iter(|| {
            // Toggle a source and re-solve — a full steady-state recompute.
            g.set_source(sources[0], 15);
            black_box(g.power(sources[0]))
        })
    });
}

criterion_group!(benches, bench_redstone);
criterion_main!(benches);
