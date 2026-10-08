//! Compare the generator stage by stage against dumps taken from the game.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_stage_parity -- <pack-root> <seed> <dump-dir> [stage...]
//! ```
//!
//! `<dump-dir>` holds, per chunk, `<cx>_<cz>.<stage>.bin` (little-endian
//! `u16` indices into `palette.txt`, ordered `y`, then `z`, then `x`, from the
//! dimension floor up) and `<cx>_<cz>.biomes.txt` (one biome id per quart,
//! ordered `qy`, `qz`, `qx`). Those come from a small harness that drives the
//! game's own `NoiseBasedChunkGenerator` over a `ProtoChunk` — see
//! `docs/KNOWN_ISSUES.md` A.8 for how the measurements were taken. Stages are
//! `noise`, `surface` and `carvers`; biomes are always checked.

#![allow(clippy::type_complexity)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use aether_world::registry::props;
use aether_worldgen::vanilla::generator::{Stage, VanillaGenerator};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: vanilla_stage_parity <pack-root> <seed> <dump-dir> [stage...]");
        std::process::exit(2);
    }
    let seed: i64 = args[1].parse().expect("seed");
    let dir = PathBuf::from(&args[2]);
    let stages: Vec<String> = if args.len() > 3 {
        args[3..].to_vec()
    } else {
        vec!["noise".into(), "surface".into(), "carvers".into()]
    };

    let gen = VanillaGenerator::new(&args[0], seed as u64).unwrap_or_else(|e| panic!("{e}"));
    if let Ok(p) = std::env::var("PROBE") {
        for pos in p.split(';') {
            let v: Vec<i32> = pos.split(',').map(|s| s.parse().unwrap()).collect();
            println!(
                "probe {pos}: biome {} prelim {}",
                gen.biome_at_block(v[0], v[1], v[2]),
                gen.terrain().preliminary_surface_level(v[0], v[2])
            );
        }
    }
    let palette: Vec<String> = std::fs::read_to_string(dir.join("palette.txt"))
        .expect("palette.txt")
        .lines()
        .map(str::to_string)
        .collect();

    let mut chunks: Vec<(i32, i32)> = std::fs::read_dir(&dir)
        .expect("dump dir")
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let base = name.strip_suffix(".biomes.txt")?;
            let (x, z) = base.split_once('_')?;
            Some((x.parse().ok()?, z.parse().ok()?))
        })
        .collect();
    chunks.sort();

    let mut totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for &(cx, cz) in &chunks {
        // Biomes.
        let want: Vec<String> = std::fs::read_to_string(dir.join(format!("{cx}_{cz}.biomes.txt")))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        let got = gen.chunk_biome_names(cx, cz);
        let bad = want.iter().zip(&got).filter(|(a, b)| a != b).count();
        let t = totals.entry("biomes".into()).or_default();
        t.0 += want.len() - bad;
        t.1 += want.len();
        if bad > 0 {
            println!("chunk {cx},{cz}: {bad} biome cells differ");
        }

        for stage_name in &stages {
            let stage = match stage_name.as_str() {
                "noise" => Stage::Noise,
                "surface" => Stage::Surface,
                "carvers" => Stage::Carvers,
                other => panic!("unknown stage {other}"),
            };
            let path = dir.join(format!("{cx}_{cz}.{stage_name}.bin"));
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let got = gen.chunk_at_stage(cx, cz, stage);
            let mut mismatch = 0usize;
            let mut examples: BTreeMap<(String, String), (usize, (i32, i32, i32))> =
                BTreeMap::new();
            for (i, w) in bytes.chunks_exact(2).enumerate() {
                let want = &palette[u16::from_le_bytes([w[0], w[1]]) as usize];
                let got_id = got[i];
                let got_name = props::state_name(got_id.0).unwrap_or_else(|| "?".into());
                if *want != got_name {
                    mismatch += 1;
                    let x = (i & 15) as i32;
                    let z = ((i >> 4) & 15) as i32;
                    let y = (i >> 8) as i32 - 64;
                    let e = examples
                        .entry((want.clone(), got_name))
                        .or_insert((0, (cx * 16 + x, y, cz * 16 + z)));
                    e.0 += 1;
                }
            }
            if let Ok(dc) = std::env::var("DEBUG_COL") {
                let (dx, dz) = dc.split_once(',').unwrap();
                let (dx, dz): (i32, i32) = (dx.parse().unwrap(), dz.parse().unwrap());
                if dx.div_euclid(16) == cx && dz.div_euclid(16) == cz {
                    let (lx, lz) = (dx.rem_euclid(16) as usize, dz.rem_euclid(16) as usize);
                    for y in (0..384usize).rev() {
                        let i = (y * 16 + lz) * 16 + lx;
                        let w =
                            &palette[u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]) as usize];
                        let g = props::state_name(got[i].0).unwrap_or_default();
                        if w != "minecraft:air" || g != "minecraft:air" {
                            println!("  {stage_name} y={:4} want {w:<36} got {g}", y as i32 - 64);
                        }
                        if y < 300
                            && w == "minecraft:stone"
                            && g == "minecraft:stone"
                            && y + 64 < 100
                        {
                            break;
                        }
                    }
                }
            }
            let n = bytes.len() / 2;
            let t = totals.entry(stage_name.clone()).or_default();
            t.0 += n - mismatch;
            t.1 += n;
            if mismatch > 0 {
                println!("chunk {cx},{cz} {stage_name}: {mismatch} / {n} differ");
                let mut ex: Vec<_> = examples.into_iter().collect();
                ex.sort_by_key(|e| std::cmp::Reverse(e.1 .0));
                for ((w, g), (count, pos)) in ex.into_iter().take(6) {
                    println!("    want {w:<40} got {g:<40} x{count} e.g. {pos:?}");
                }
            }
        }
    }
    for (k, (ok, n)) in totals {
        println!(
            "{k:>8}: {ok} / {n} match ({:.4}%)",
            ok as f64 * 100.0 / n.max(1) as f64
        );
    }
}
