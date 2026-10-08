//! Time chunk generation, and say where the time goes.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_bench -- <pack-root> <seed> [columns] [biome-report]
//! ```

use std::time::Instant;

use aether_worldgen::vanilla::generator::VanillaGenerator;
use aether_worldgen::vanilla::terrain::Terrain;
use aether_worldgen::ChunkGenerator;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: vanilla_bench <pack-root> <seed> [columns] [biome-report]");
        std::process::exit(2);
    }
    let seed: i64 = args[1].parse().expect("seed");
    let n: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let biome_report = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "/tmp/vanilla-ref/generated/reports/biome_parameters/minecraft/overworld.json".to_string());

    let t0 = Instant::now();
    let gen = VanillaGenerator::load(&args[0], &biome_report, seed as u64)
        .unwrap_or_else(|e| panic!("{e}"));
    println!("load: {:?}", t0.elapsed());

    let terrain = Terrain::load(&args[0], seed as u64).unwrap_or_else(|e| panic!("{e}"));
    let s = terrain.settings();

    // One raw graph walk, for scale.
    let t = Instant::now();
    let mut sink = 0.0f64;
    for i in 0..200 {
        sink += terrain.density_at(i * 7, 30 + (i % 40), i * -3);
    }
    println!(
        "density_at, scattered: {:?} each ({sink:.3})",
        t.elapsed() / 200
    );

    // A column scan, which is what generation actually does.
    let t = Instant::now();
    let mut sink = 0.0f64;
    for y in 0..s.height {
        sink += terrain.density_at(1000, s.min_y + y, 1000);
    }
    println!(
        "density_at, one column of {} blocks: {:?} total, {:?} each ({sink:.3})",
        s.height,
        t.elapsed(),
        t.elapsed() / s.height as u32
    );

    let t = Instant::now();
    let chunk = terrain.chunk(62, 62);
    let mut solid = 0usize;
    for y in 0..s.height {
        if chunk.block_at(992, s.min_y + y, 992) != aether_worldgen::vanilla::terrain::NoiseBlock::Air
        {
            solid += 1;
        }
    }
    println!(
        "block_at, one column: {:?} ({solid} non-air)",
        t.elapsed()
    );

    let t = Instant::now();
    let mut sections = 0usize;
    for i in 0..n {
        let col = gen.generate_column(i, 0);
        sections += col.sections.len();
    }
    let d = t.elapsed();
    println!(
        "generate_column x{n}: {:?} total, {:?} per column, {:.3} columns/s ({sections} sections)",
        d,
        d / n as u32,
        n as f64 / d.as_secs_f64()
    );
}
