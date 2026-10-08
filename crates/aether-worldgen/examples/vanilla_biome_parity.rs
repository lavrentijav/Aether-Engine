//! Measure this crate's biome output against a world vanilla itself generated.
//!
//! Every 4×4×4 biome cell of every fully-generated chunk in a reference world
//! is compared against [`OverworldBiomeSource`]. The answer is a count, not an
//! impression: "N of M cells agree".
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_biome_parity -- \
//!     <region-dir> <pack-root> <biome-report.json> <seed> [max-chunks]
//! ```
//!
//! * `<region-dir>` — `world/dimensions/minecraft/overworld/region`
//! * `<pack-root>` — a directory holding `data/minecraft/worldgen/` (an
//!   unpacked server jar)
//! * `<biome-report.json>` — `reports/biome_parameters/minecraft/overworld.json`
//!   from a `--reports` run
//!
//! # Reading the region file
//!
//! The biome container is decoded straight from the Anvil layout — a palette
//! plus indices packed into `i64`s without straddling word boundaries — and
//! *not* by running this crate's own writer backwards. A decoder mirrored off
//! an encoder agrees with it by construction and proves nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aether_convert::nbt::{self, Nbt};
use aether_worldgen::vanilla::climate::SearchCache;
use aether_worldgen::vanilla::overworld::OverworldBiomeSource;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!(
            "usage: vanilla_biome_parity <region-dir> <pack-root> <biome-report.json> <seed> [max-chunks]"
        );
        std::process::exit(2);
    }
    let region_dir = PathBuf::from(&args[0]);
    let seed: i64 = args[3].parse().expect("seed must be an integer");
    let max_chunks: usize = args
        .get(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);

    let source = OverworldBiomeSource::load(&args[1], &args[2], seed as u64)
        .unwrap_or_else(|e| panic!("cannot build the biome source: {e}"));
    println!("biome table rows: {}", source.biomes().len());

    let mut regions: Vec<PathBuf> = std::fs::read_dir(&region_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", region_dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "mca"))
        .collect();
    regions.sort();

    let mut cells = 0usize;
    let mut agree = 0usize;
    let mut chunks = 0usize;
    // Mismatches, counted by (vanilla biome, ours).
    let mut confusion: BTreeMap<(String, String), usize> = BTreeMap::new();
    // How many disagreements sat on an exact distance tie, where vanilla's
    // R-tree order legitimately decides and a linear scan cannot.
    let mut tied_mismatches = 0usize;
    let mut example: Option<(i32, i32, i32, String, String)> = None;
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    // Vanilla's biome search remembers its previous answer per thread, and that
    // memory decides exact distance ties. Replaying a cache in the order
    // vanilla fills a chunk (sections upward, then x, y, z) is the closest an
    // outside observer can get to that state; measured next to the cache-free
    // answer so the difference, if any, is visible.
    let mut cache = SearchCache::new();
    let mut cached_agree = 0usize;
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();

    'outer: for path in &regions {
        let data = std::fs::read(path).expect("read region");
        let (rx, rz) = region_coords(path);
        for idx in 0..1024usize {
            let Some(payload) = chunk_payload(&data, idx) else {
                continue;
            };
            let Ok(root) = nbt::parse(&payload) else {
                continue;
            };
            // Biomes are assigned once, at the `biomes` generation step, and
            // no later step revises them — so every chunk at or past that step
            // carries its *final* biomes even though it may still be missing
            // blocks. Chunks short of it carry nothing to compare, and
            // including them would inflate the count with empty agreement.
            let status = match get(&root, "Status") {
                Some(Nbt::String(s)) => s.as_str(),
                _ => continue,
            };
            if !BIOMES_FINAL.contains(&status) {
                *skipped.entry(status.to_string()).or_default() += 1;
                continue;
            }
            *by_status.entry(status.to_string()).or_default() += 1;
            let cx = rx * 32 + (idx % 32) as i32;
            let cz = rz * 32 + (idx / 32) as i32;
            chunks += 1;

            let Some(Nbt::List(sections)) = get(&root, "sections") else {
                continue;
            };
            for sec in sections {
                let Some(Nbt::Byte(sy)) = get(sec, "Y") else {
                    continue;
                };
                let sy = *sy as i32;
                let Some(biomes) = decode_biomes(sec) else {
                    continue;
                };
                for bx in 0..4i32 {
                    for by in 0..4i32 {
                        for bz in 0..4i32 {
                            let want = &biomes[(by * 16 + bz * 4 + bx) as usize];
                            let qx = cx * 4 + bx;
                            let qy = sy * 4 + by;
                            let qz = cz * 4 + bz;
                            let got = source.biome_at(qx, qy, qz);
                            if source.biome_at_cached(qx, qy, qz, &mut cache) == want.as_str() {
                                cached_agree += 1;
                            }
                            cells += 1;
                            if got == want {
                                agree += 1;
                            } else {
                                // Only worth the extra scan on the rare cell
                                // that disagrees.
                                if source.biome_at_with_ties(qx, qy, qz).1 > 1 {
                                    tied_mismatches += 1;
                                }
                                *confusion
                                    .entry((want.clone(), got.to_string()))
                                    .or_default() += 1;
                                if example.is_none() {
                                    example = Some((qx, qy, qz, want.clone(), got.to_string()));
                                }
                            }
                        }
                    }
                }
            }
            if chunks >= max_chunks {
                break 'outer;
            }
        }
    }

    println!("chunks compared: {chunks}");
    for (st, n) in &by_status {
        println!("  {n:>5}  at status {st}");
    }
    for (st, n) in &skipped {
        println!("  {n:>5}  skipped, still at {st} (no biomes yet)");
    }
    println!(
        "biome cells: {agree}/{cells} agree ({:.4}%)",
        100.0 * agree as f64 / cells.max(1) as f64
    );
    println!(
        "             {cached_agree}/{cells} agree replaying a search cache ({:.4}%)",
        100.0 * cached_agree as f64 / cells.max(1) as f64
    );
    if agree != cells {
        println!("of the {} disagreements, {tied_mismatches} sat on an exact distance tie",
            cells - agree);
        println!("\ntop confusions (vanilla -> ours):");
        let mut v: Vec<_> = confusion.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for ((want, got), n) in v.into_iter().take(15) {
            println!("  {n:>7}  {want} -> {got}");
        }
        if let Some((x, y, z, want, got)) = example {
            println!("\nfirst mismatch at quart ({x},{y},{z}): vanilla {want}, ours {got}");
        }
    }
}

/// Chunk statuses at or past `biomes`, i.e. the ones whose biome containers
/// hold what vanilla finally decided. Taken from vanilla's status ladder:
/// empty, structure_starts, structure_references, **biomes**, noise, surface,
/// carvers, features, initialize_light, light, spawn, full.
const BIOMES_FINAL: [&str; 9] = [
    "minecraft:biomes",
    "minecraft:noise",
    "minecraft:surface",
    "minecraft:carvers",
    "minecraft:features",
    "minecraft:initialize_light",
    "minecraft:light",
    "minecraft:spawn",
    "minecraft:full",
];

fn region_coords(path: &Path) -> (i32, i32) {
    let name = path.file_name().unwrap().to_str().unwrap();
    let parts: Vec<&str> = name.split('.').collect();
    (parts[1].parse().unwrap(), parts[2].parse().unwrap())
}

fn chunk_payload(data: &[u8], idx: usize) -> Option<Vec<u8>> {
    let e = idx * 4;
    let offset = ((data[e] as usize) << 16) | ((data[e + 1] as usize) << 8) | (data[e + 2] as usize);
    let sectors = data[e + 3] as usize;
    if offset == 0 || sectors == 0 {
        return None;
    }
    let start = offset * 4096;
    let len = u32::from_be_bytes(data.get(start..start + 4)?.try_into().ok()?) as usize;
    let scheme = *data.get(start + 4)?;
    let body = data.get(start + 5..start + 4 + len)?;
    match scheme {
        2 => miniz_oxide::inflate::decompress_to_vec_zlib(body).ok(),
        3 => Some(body.to_vec()),
        _ => None,
    }
}

/// The 64 biome ids of one section, in Anvil's `y*16 + z*4 + x` order.
///
/// Anvil packs palette indices into `i64` words at `max(1, ceil(log2(n)))`
/// bits each, never letting an index straddle two words, so a word holds
/// `64 / bits` of them and the spare high bits are ignored. A single-entry
/// palette carries no `data` at all.
fn decode_biomes(section: &Nbt) -> Option<Vec<String>> {
    let container = get(section, "biomes")?;
    let Nbt::List(palette) = get(container, "palette")? else {
        return None;
    };
    let names: Vec<String> = palette
        .iter()
        .map(|e| match e {
            Nbt::String(s) => Some(s.clone()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if names.is_empty() {
        return None;
    }
    if names.len() == 1 {
        return Some(vec![names[0].clone(); 64]);
    }
    let Nbt::LongArray(words) = get(container, "data")? else {
        return None;
    };
    let bits = (usize::BITS - (names.len() - 1).leading_zeros()) as usize;
    let bits = bits.max(1);
    let per_word = 64 / bits;
    let mask = (1u64 << bits) - 1;
    let mut out = Vec::with_capacity(64);
    for i in 0..64usize {
        let w = words.get(i / per_word)?;
        let shift = (i % per_word) * bits;
        let index = ((*w as u64) >> shift) & mask;
        out.push(names.get(index as usize)?.clone());
    }
    Some(out)
}

fn get<'a>(v: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    let Nbt::Compound(fields) = v else {
        return None;
    };
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}
