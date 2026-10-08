//! Compare biome selection against the game's own `MultiNoiseBiomeSource`,
//! position by position.
//!
//! This is a sharper instrument than [`vanilla_biome_parity`], which reads a
//! generated world: a world only covers the chunks someone bothered to
//! generate, while a dump can sweep as much of the climate space as you like —
//! including the boundaries between biomes, where the nearest-entry search is
//! most likely to disagree.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_biome_source_parity -- \
//!     <pack-root> <biome-report.json> <seed> <dump-file>
//! ```
//!
//! # Where the dump comes from
//!
//! A small Java program linked against the operator's own server jar, one line
//! per position — `<quart-x> <quart-y> <quart-z> <biome-id>`:
//!
//! ```java
//! RandomState rs = RandomState.create(provider, NoiseGeneratorSettings.OVERWORLD, seed);
//! var list = MultiNoiseBiomeSourceParameterList.knownPresets()
//!     .get(MultiNoiseBiomeSourceParameterList.Preset.OVERWORLD);
//! list.findValue(rs.sampler().sample(qx, qy, qz));
//! ```
//!
//! # What a disagreement means
//!
//! Vanilla searches its biome table with an R-tree; this crate scans the table
//! linearly. Both find the true nearest entry, so they can only differ where
//! two entries are *exactly* equidistant and the tie is broken by table order.
//! The report separates those cases out: ties are a known, bounded ambiguity,
//! anything else is a real bug.

use std::collections::BTreeMap;

use aether_worldgen::vanilla::climate::SearchCache;
use aether_worldgen::vanilla::overworld::OverworldBiomeSource;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!(
            "usage: vanilla_biome_source_parity <pack-root> <biome-report.json> <seed> <dump-file>"
        );
        std::process::exit(2);
    }
    let seed: i64 = args[2].parse().expect("seed must be an integer");
    let source = OverworldBiomeSource::load(&args[0], &args[1], seed as u64)
        .unwrap_or_else(|e| panic!("{e}"));
    println!("biome table rows: {}", source.biomes().len());

    let dump = std::fs::read_to_string(&args[3]).unwrap_or_else(|e| panic!("{}: {e}", args[3]));
    let mut total = 0usize;
    let mut agree = 0usize;
    let mut tied_mismatches = 0usize;
    let mut confusion: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut first_bad: Option<(i32, i32, i32, String, String)> = None;
    // The dump was produced by one thread making these lookups in order, so
    // feeding one cache through in the same order mirrors it exactly. Measured
    // alongside the cache-free path to show whether the history matters here.
    let mut cache = SearchCache::new();
    let mut cached_agree = 0usize;

    for line in dump.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 4 {
            continue;
        }
        let (x, y, z) = (
            f[0].parse::<i32>().unwrap(),
            f[1].parse::<i32>().unwrap(),
            f[2].parse::<i32>().unwrap(),
        );
        let want = f[3];
        let got = source.biome_at(x, y, z);
        if source.biome_at_cached(x, y, z, &mut cache) == want {
            cached_agree += 1;
        }
        total += 1;
        if got == want {
            agree += 1;
        } else {
            if source.biome_at_with_ties(x, y, z).1 > 1 {
                tied_mismatches += 1;
            }
            *confusion
                .entry((want.to_string(), got.to_string()))
                .or_default() += 1;
            if first_bad.is_none() {
                first_bad = Some((x, y, z, want.to_string(), got.to_string()));
            }
        }
    }

    println!(
        "\n{agree}/{total} biomes agree, cache-free ({:.4}%)",
        100.0 * agree as f64 / total.max(1) as f64
    );
    println!(
        "{cached_agree}/{total} biomes agree, replaying the search cache ({:.4}%)",
        100.0 * cached_agree as f64 / total.max(1) as f64
    );
    if agree != total {
        println!(
            "{tied_mismatches} of the {} disagreements sat on an exact distance tie",
            total - agree
        );
        let mut v: Vec<_> = confusion.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        println!("\ntop confusions (vanilla -> ours):");
        for ((want, got), n) in v.into_iter().take(15) {
            println!("  {n:>7}  {want} -> {got}");
        }
        if let Some((x, y, z, want, got)) = first_bad {
            println!("\nfirst mismatch at quart ({x},{y},{z}): vanilla {want}, ours {got}");
        }
    }
}
