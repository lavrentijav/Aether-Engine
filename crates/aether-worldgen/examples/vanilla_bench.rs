//! Time chunk generation, and say where the time goes.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_bench -- <pack-root> <seed> [chunks]
//! ```
//!
//! Each stage is timed on its own over a row of chunks (stages include the
//! ones before them), then full columns — decoration included — are timed
//! over a square, the way a joining player's view fills.

use std::time::Instant;

use aether_worldgen::vanilla::generator::{Stage, VanillaGenerator};
use aether_worldgen::ChunkGenerator;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: vanilla_bench <pack-root> <seed> [chunks]");
        std::process::exit(2);
    }
    let seed: i64 = args[1].parse().expect("seed");
    let n: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);

    let t0 = Instant::now();
    let gen = VanillaGenerator::new(&args[0], seed as u64).unwrap_or_else(|e| panic!("{e}"));
    println!("load: {:?}", t0.elapsed());

    for (name, stage) in [
        ("biomes", Stage::Biomes),
        ("noise", Stage::Noise),
        ("surface", Stage::Surface),
        ("carvers", Stage::Carvers),
    ] {
        let t = Instant::now();
        for i in 0..n {
            let _ = gen.proto_chunk(100 + i, 50, stage);
        }
        println!("{name:>8} (cumulative): {:?} per chunk", t.elapsed() / n as u32);
    }

    let side = (n as f64).sqrt().ceil() as i32 + 2;
    let t = Instant::now();
    for z in 0..side {
        for x in 0..side {
            let _ = gen.generate_column(x - 300, z - 300);
        }
    }
    let cols = (side * side) as u32;
    println!(
        "full columns, {side}x{side} square, single thread: {:?} total, {:?} per column",
        t.elapsed(),
        t.elapsed() / cols
    );
}
