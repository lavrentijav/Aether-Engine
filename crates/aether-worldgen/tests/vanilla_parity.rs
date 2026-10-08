//! Parity against a world vanilla generated, when the operator's copy of the
//! game is on hand.
//!
//! These tests need three things this repository deliberately does not ship:
//! an unpacked copy of the game's worldgen data, a `--reports` biome-parameter
//! dump, and a reference world generated at a known seed. Point the
//! environment at them and the tests run; leave it unset and they skip, so
//! `cargo test` stays green on a machine that has none of it.
//!
//! The terrain test additionally needs a column dump from the game (see
//! `examples/vanilla_terrain_parity.rs` for how it is produced) and is skipped
//! without one.
//!
//! ```text
//! AETHER_VANILLA_PACK=/path/to/unpacked-jar \
//! AETHER_VANILLA_BIOME_REPORT=/path/to/reports/biome_parameters/minecraft/overworld.json \
//! AETHER_VANILLA_REGION=/path/to/world/dimensions/minecraft/overworld/region \
//! AETHER_VANILLA_SEED=42 \
//! AETHER_VANILLA_COLUMN_DUMP=/path/to/columns.txt \
//! cargo test -p aether-worldgen --test vanilla_parity -- --nocapture
//! ```

use aether_worldgen::vanilla::climate::SearchCache;
use aether_worldgen::vanilla::overworld::OverworldBiomeSource;

struct Env {
    pack: String,
    report: String,
    region: String,
    seed: u64,
}

/// The reference data, or `None` when this machine has not been pointed at a
/// copy of the game.
fn env() -> Option<Env> {
    Some(Env {
        pack: std::env::var("AETHER_VANILLA_PACK").ok()?,
        report: std::env::var("AETHER_VANILLA_BIOME_REPORT").ok()?,
        region: std::env::var("AETHER_VANILLA_REGION").ok()?,
        seed: std::env::var("AETHER_VANILLA_SEED")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(42) as u64,
    })
}

#[test]
fn biomes_match_the_reference_world() {
    let Some(e) = env() else {
        eprintln!("skipping: AETHER_VANILLA_* not set");
        return;
    };
    let source = OverworldBiomeSource::load(&e.pack, &e.report, e.seed)
        .unwrap_or_else(|err| panic!("cannot build the biome source: {err}"));

    let r = compare(&e.region, &source);
    assert!(
        r.chunks > 0,
        "no chunk in {} had reached the `biomes` generation step — nothing was compared",
        e.region
    );
    eprintln!(
        "{}/{} biome cells agree across {} chunks ({} cache-free)",
        r.cached_agree, r.cells, r.chunks, r.agree
    );
    // The cached form is the exact one: vanilla's search remembers its previous
    // answer, and that memory settles exact distance ties. Replaying a cache in
    // the order vanilla fills a chunk reproduces those. The cache-free number is
    // reported too, because it is the one the ordinary API gives — it may
    // differ on a handful of tied cells, and that is a known ambiguity rather
    // than a defect.
    assert_eq!(
        r.cached_agree,
        r.cells,
        "{} of {} biome cells disagree with vanilla",
        r.cells - r.cached_agree,
        r.cells
    );
}

/// Chunk statuses at or past `biomes`; earlier ones have no biomes to compare.
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

/// What one pass over a reference world measured.
struct Measured {
    cells: usize,
    /// Cells matched by the order-independent lookup.
    agree: usize,
    /// Cells matched when vanilla's search cache is replayed in fill order.
    cached_agree: usize,
    chunks: usize,
}

fn compare(region_dir: &str, source: &OverworldBiomeSource) -> Measured {
    use aether_convert::nbt::{self, Nbt};

    let mut cells = 0;
    let mut agree = 0;
    let mut cached_agree = 0;
    let mut chunks = 0;
    let mut cache = SearchCache::new();

    let mut paths: Vec<_> = std::fs::read_dir(region_dir)
        .unwrap_or_else(|e| panic!("cannot read {region_dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "mca"))
        .collect();
    paths.sort();

    for path in paths {
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let parts: Vec<&str> = name.split('.').collect();
        let (rx, rz): (i32, i32) = (parts[1].parse().unwrap(), parts[2].parse().unwrap());
        let data = std::fs::read(&path).unwrap();
        for idx in 0..1024usize {
            let e = idx * 4;
            let offset =
                ((data[e] as usize) << 16) | ((data[e + 1] as usize) << 8) | (data[e + 2] as usize);
            if offset == 0 || data[e + 3] == 0 {
                continue;
            }
            let start = offset * 4096;
            let len = u32::from_be_bytes(data[start..start + 4].try_into().unwrap()) as usize;
            let body = &data[start + 5..start + 4 + len];
            let payload = match data[start + 4] {
                2 => match miniz_oxide::inflate::decompress_to_vec_zlib(body) {
                    Ok(v) => v,
                    Err(_) => continue,
                },
                3 => body.to_vec(),
                _ => continue,
            };
            let Ok(root) = nbt::parse(&payload) else {
                continue;
            };
            match get(&root, "Status") {
                Some(Nbt::String(s)) if BIOMES_FINAL.contains(&s.as_str()) => {}
                _ => continue,
            }
            chunks += 1;
            let cx = rx * 32 + (idx % 32) as i32;
            let cz = rz * 32 + (idx / 32) as i32;
            let Some(Nbt::List(sections)) = get(&root, "sections") else {
                continue;
            };
            for sec in sections {
                let Some(Nbt::Byte(sy)) = get(sec, "Y") else {
                    continue;
                };
                let sy = *sy as i32;
                let Some(names) = decode_biomes(sec) else {
                    continue;
                };
                // Vanilla's own fill order: x outermost, then y, then z.
                for bx in 0..4i32 {
                    for by in 0..4i32 {
                        for bz in 0..4i32 {
                            let want = &names[(by * 16 + bz * 4 + bx) as usize];
                            let (qx, qy, qz) = (cx * 4 + bx, sy * 4 + by, cz * 4 + bz);
                            cells += 1;
                            if source.biome_at(qx, qy, qz) == want {
                                agree += 1;
                            }
                            if source.biome_at_cached(qx, qy, qz, &mut cache) == want {
                                cached_agree += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    Measured {
        cells,
        agree,
        cached_agree,
        chunks,
    }
}

/// One section's 64 biome cells, decoded from the Anvil palette container.
fn decode_biomes(section: &aether_convert::nbt::Nbt) -> Option<Vec<String>> {
    use aether_convert::nbt::Nbt;
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
    // Indices are packed at ceil(log2(n)) bits, never straddling a word.
    let bits = ((usize::BITS - (names.len() - 1).leading_zeros()) as usize).max(1);
    let per_word = 64 / bits;
    let mask = (1u64 << bits) - 1;
    (0..64)
        .map(|i| {
            let w = *words.get(i / per_word)? as u64;
            let index = (w >> ((i % per_word) * bits)) & mask;
            names.get(index as usize).cloned()
        })
        .collect()
}

fn get<'a>(v: &'a aether_convert::nbt::Nbt, key: &str) -> Option<&'a aether_convert::nbt::Nbt> {
    let aether_convert::nbt::Nbt::Compound(fields) = v else {
        return None;
    };
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Every block of the noise stage — cell interpolation, aquifers and ore veins
/// — against a column dump taken from the game's own
/// `NoiseBasedChunkGenerator.getBaseColumn`.
#[test]
fn terrain_matches_the_reference_columns() {
    use aether_worldgen::vanilla::terrain::Terrain;

    let Some(e) = env() else {
        eprintln!("skipping: AETHER_VANILLA_* not set");
        return;
    };
    let Ok(dump_path) = std::env::var("AETHER_VANILLA_COLUMN_DUMP") else {
        eprintln!("skipping: AETHER_VANILLA_COLUMN_DUMP not set");
        return;
    };
    let dump =
        std::fs::read_to_string(&dump_path).unwrap_or_else(|err| panic!("{dump_path}: {err}"));
    let terrain =
        Terrain::load(&e.pack, e.seed).unwrap_or_else(|err| panic!("cannot load terrain: {err}"));
    let s = terrain.settings();

    let mut blocks = 0usize;
    let mut agree = 0usize;
    let mut first_bad: Option<(i32, i32, i32, String, &'static str)> = None;

    for line in dump.lines() {
        let mut it = line.split_whitespace();
        let (Some(x), Some(z)) = (it.next(), it.next()) else {
            continue;
        };
        let (x, z) = (x.parse::<i32>().unwrap(), z.parse::<i32>().unwrap());
        // Per chunk, because the aquifer is: one of its cut-offs comes from
        // the whole chunk.
        let chunk = terrain.chunk(x.div_euclid(16), z.div_euclid(16));
        let mut y = s.min_y;
        for run in it {
            let (n, name) = run.split_once('*').expect("run-length entry");
            let n: i32 = n.parse().unwrap();
            for yy in y..y + n {
                blocks += 1;
                let got = chunk.block_at(x, yy, z);
                if got.name() == name {
                    agree += 1;
                } else if first_bad.is_none() {
                    first_bad = Some((x, yy, z, name.to_string(), got.name()));
                }
            }
            y += n;
        }
    }

    assert!(blocks > 0, "{dump_path} held no columns");
    eprintln!("{agree}/{blocks} noise-stage blocks agree");
    if let Some((x, y, z, want, got)) = first_bad {
        panic!(
            "{} of {blocks} blocks disagree; first at ({x},{y},{z}): vanilla {want}, ours {got}",
            blocks - agree
        );
    }
}
