//! The per-player visibility breadth-first search over sections.
//!
//! The graph itself is cached per section by the server, so the benchmark
//! measures the search given a cheap graph lookup — a hash set of open
//! sections — which is what a re-run after a section-border crossing costs
//! once the graphs are warm.

use std::collections::HashSet;

use aether_world::visibility::{reachable_sections, SectionGraph};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// A block of open sections around the origin with a few sealed walls, the
/// shape a re-run sees: pockets of air with solid rock between them.
fn open_sections() -> HashSet<(i32, i32, i32)> {
    let mut open = HashSet::new();
    for x in -12..=12 {
        for y in -4..=4 {
            for z in -12..=12 {
                // A vertical wall every six sections, with a doorway at x%6.
                let wall = x % 6 == 0;
                let doorway = y == 0 && z % 5 == 0;
                if !wall || doorway {
                    open.insert((x, y, z));
                }
            }
        }
    }
    open
}

fn bench_visibility(c: &mut Criterion) {
    let open = open_sections();
    let provider = |x: i32, y: i32, z: i32| {
        Some(if open.contains(&(x, y, z)) {
            SectionGraph::open()
        } else {
            SectionGraph::sealed()
        })
    };
    let mut provider = provider;

    c.bench_function("reachable_sections_view8", |b| {
        b.iter(|| {
            reachable_sections(
                black_box((0.5, 70.5, 0.5)),
                black_box(5.0),
                black_box(8),
                black_box(-4),
                black_box(4),
                &mut provider,
            )
        })
    });
}

criterion_group!(benches, bench_visibility);
criterion_main!(benches);
