//! Compare the terrain silhouette — which blocks are solid — against the game's
//! own noise stage.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_terrain_parity -- \
//!     <pack-root> <seed> <column-dump> [<density-dump>]
//! ```
//!
//! Two measurements, in increasing order of sharpness:
//!
//! * the **column dump** says which block vanilla placed, so it measures the
//!   terrain silhouette but folds in aquifers and ore veins, which are not
//!   implemented here;
//! * the **density dump** is the interpolated `final_density` itself, bit for
//!   bit — the number cell interpolation is responsible for and nothing else.
//!
//! # Where the dump comes from
//!
//! `NoiseBasedChunkGenerator.getBaseColumn` returns exactly what
//! `fillFromNoise` would place in a column: cell-interpolated density, aquifers
//! and ore veins, and nothing after that — no surface rules, no carvers, no
//! features. A small Java program linked against the operator's own server jar
//! prints one column per line, run-length encoded from `min_y` upward:
//!
//! ```text
//! <x> <z> 22*minecraft:stone 3*minecraft:air 111*minecraft:stone 248*minecraft:air
//! ```
//!
//! ```java
//! NoiseBasedChunkGenerator gen = new NoiseBasedChunkGenerator(biomeSource, settings);
//! RandomState rs = RandomState.create(settings.value(), noiseGetter, seed);
//! NoiseColumn col = gen.getBaseColumn(x, z, LevelHeightAccessor.create(-64, 384), rs);
//! ```
//!
//! The density dump comes from a second Java class placed *in vanilla's own
//! package*, so that it can reach `NoiseChunk.getInterpolatedDensity()`. It
//! drives the interpolator exactly the way `iterateNoiseColumn` does and prints
//! one raw `double` bit pattern per block, `min_y` upward:
//!
//! ```java
//! NoiseChunk nc = new NoiseChunk(1, rs, x, z, noiseSettings,
//!     DensityFunctions.BeardifierMarker.INSTANCE, ngs, fluidPicker, Blender.empty());
//! nc.initializeForFirstCellX();
//! nc.advanceCellX(0);
//! nc.selectCellYZ(cellY, 0);
//! nc.updateForY(y, inCellY / (double) cellHeight);
//! nc.updateForX(x, Math.floorMod(x, cellWidth) / (double) cellWidth);
//! nc.updateForZ(z, Math.floorMod(z, cellWidth) / (double) cellWidth);
//! nc.getInterpolatedDensity();
//! ```
//!
//! # What is compared
//!
//! The exact block, per position. Two coarser numbers sit alongside it because
//! they localize a failure: *density alone* ignores the aquifer's stone
//! barriers, so the gap between it and the full number is the barriers' doing;
//! and the *interpolated density* comparison is upstream of every block
//! decision, so if that diverges nothing downstream is meaningful.

#![allow(clippy::type_complexity)]

use std::collections::BTreeMap;

use aether_worldgen::vanilla::terrain::Terrain;

/// The chunk a block column belongs to.
fn chunk_of(x: i32, z: i32) -> (i32, i32) {
    (x.div_euclid(16), z.div_euclid(16))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!(
            "usage: vanilla_terrain_parity <pack-root> <seed> <column-dump> [<density-dump>]"
        );
        std::process::exit(2);
    }
    let seed: i64 = args[1].parse().expect("seed must be an integer");
    let terrain = Terrain::load(&args[0], seed as u64).unwrap_or_else(|e| panic!("{e}"));
    let s = terrain.settings();
    println!(
        "cells {}x{}x{}, y {}..{}",
        s.cell_width,
        s.cell_height,
        s.cell_width,
        s.min_y,
        s.max_y()
    );

    if let Some(path) = args.get(3) {
        compare_density(&terrain, path);
    }

    let dump = std::fs::read_to_string(&args[2]).unwrap_or_else(|e| panic!("{}: {e}", args[2]));
    let mut blocks = 0usize;
    let mut exact = 0usize;
    let mut solid_agree = 0usize;
    let mut columns = 0usize;
    let mut columns_exact = 0usize;
    let mut confusion: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
    let mut first_bad: Option<(i32, i32, i32, String, &'static str)> = None;
    let mut worst: Option<(i32, i32, usize)> = None;

    for line in dump.lines() {
        let mut it = line.split_whitespace();
        let (Some(x), Some(z)) = (it.next(), it.next()) else {
            continue;
        };
        let (x, z) = (x.parse::<i32>().unwrap(), z.parse::<i32>().unwrap());
        columns += 1;
        let (cx, cz) = chunk_of(x, z);
        let chunk = terrain.chunk(cx, cz);
        let mut y = s.min_y;
        let mut bad_here = 0usize;
        for run in it {
            let (n, name) = run.split_once('*').expect("run-length entry");
            let n: i32 = n.parse().unwrap();
            let want_solid =
                !matches!(name, "minecraft:air" | "minecraft:water" | "minecraft:lava");
            for yy in y..y + n {
                blocks += 1;
                let got = chunk.block_at(x, yy, z);
                if got.name() == name {
                    exact += 1;
                } else {
                    bad_here += 1;
                    *confusion.entry((name.to_string(), got.name())).or_default() += 1;
                    if first_bad.is_none() {
                        first_bad = Some((x, yy, z, name.to_string(), got.name()));
                    }
                }
                // The density on its own, with no aquifer barrier applied.
                if terrain.is_solid(x, yy, z) == want_solid {
                    solid_agree += 1;
                }
            }
            y += n;
        }
        if bad_here == 0 {
            columns_exact += 1;
        } else if worst.map(|(_, _, n)| bad_here > n).unwrap_or(true) {
            worst = Some((x, z, bad_here));
        }
    }

    println!("columns: {columns}");
    println!(
        "exact block: {exact}/{blocks} ({:.6}%)",
        100.0 * exact as f64 / blocks.max(1) as f64
    );
    println!("columns identical top to bottom: {columns_exact}/{columns}");
    println!("  (density alone, ignoring the aquifer's stone barriers: {solid_agree}/{blocks})");
    if exact != blocks {
        println!("\ndisagreements (vanilla -> ours):");
        let mut v: Vec<_> = confusion.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for ((want, got), n) in v.into_iter().take(15) {
            println!("  {n:>8}  {want} -> {got}");
        }
        if let Some((x, y, z, want, got)) = first_bad {
            println!(
                "\nfirst divergence at ({x},{y},{z}): vanilla {want}, ours {got}; density {:.17e}",
                terrain.density_at(x, y, z)
            );
        }
        if let Some((x, z, n)) = worst {
            println!("worst column ({x},{z}): {n} blocks differ");
        }
    }
}

/// The sharp measurement: the interpolated density itself, bit for bit.
fn compare_density(terrain: &Terrain, path: &str) {
    let s = terrain.settings();
    let dump = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut total = 0usize;
    let mut agree = 0usize;
    let mut columns = 0usize;
    let mut first_bad: Option<(i32, i32, i32, u64, u64)> = None;

    for line in dump.lines() {
        let mut it = line.split_whitespace();
        let (Some(x), Some(z)) = (it.next(), it.next()) else {
            continue;
        };
        let (x, z) = (x.parse::<i32>().unwrap(), z.parse::<i32>().unwrap());
        columns += 1;
        for (i, field) in it.enumerate() {
            let y = s.min_y + i as i32;
            let want = u64::from_str_radix(field, 16).unwrap();
            let got = terrain.density_at(x, y, z).to_bits();
            total += 1;
            if got == want {
                agree += 1;
            } else if first_bad.is_none() {
                first_bad = Some((x, y, z, want, got));
            }
        }
    }
    println!("interpolated density: {agree}/{total} values bit-identical across {columns} columns");
    if let Some((x, y, z, want, got)) = first_bad {
        println!(
            "  first divergence at ({x},{y},{z}): vanilla {:.17e}, ours {:.17e}",
            f64::from_bits(want),
            f64::from_bits(got)
        );
    }
    println!();
}
