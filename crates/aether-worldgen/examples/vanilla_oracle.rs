//! Read biomes and blocks back out of a world vanilla itself generated.
//!
//! This is the measuring stick for parity work: a generator can only be said
//! to match vanilla if its output is compared against vanilla's, and the only
//! honest source of that is a world the real server wrote. Tests that check a
//! generator against a second copy of its own logic prove nothing.
//!
//! Usage:
//!
//! ```text
//! cargo run -p aether-worldgen --example vanilla_oracle -- <region-dir> [samples]
//! ```
//!
//! `<region-dir>` is the `region/` folder of a generated world — for modern
//! versions that is `world/dimensions/minecraft/overworld/region`.

#![allow(clippy::type_complexity)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aether_convert::nbt::{self, Nbt};

/// One column sampled out of the reference world.
#[derive(Debug)]
struct Sample {
    x: i32,
    z: i32,
    /// Biome at the sampled quart position, as a namespaced id.
    biome: String,
    /// Y of the topmost non-air block, or `None` for an empty column.
    surface_y: Option<i32>,
    /// The block at that surface.
    surface_block: Option<String>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().unwrap_or_else(|| {
        eprintln!("usage: vanilla_oracle <region-dir> [samples]");
        std::process::exit(2);
    }));
    let want: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(16);

    let mut regions: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "mca"))
        .collect();
    regions.sort();
    println!("regions: {}", regions.len());

    let mut samples = Vec::new();
    let mut biome_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut chunks_read = 0usize;

    'outer: for path in &regions {
        let data = std::fs::read(path).expect("read region");
        let (rx, rz) = region_coords(path);
        for idx in 0..1024usize {
            let Some(payload) = chunk_payload(&data, idx) else {
                continue;
            };
            let Ok(root) = nbt::parse(&payload) else {
                eprintln!("chunk {idx} in {}: NBT parse failed", path.display());
                continue;
            };
            // Only fully generated chunks are usable as a reference. Vanilla
            // also writes partially generated ones — those carry biomes but
            // no blocks yet, and sampling them reads an empty world.
            match compound_get(&root, "Status") {
                Some(Nbt::String(s)) if s == "minecraft:full" => {}
                _ => continue,
            }
            chunks_read += 1;
            let cx = rx * 32 + (idx % 32) as i32;
            let cz = rz * 32 + (idx / 32) as i32;
            if let Some(s) = sample_column(&root, cx, cz) {
                *biome_counts.entry(s.biome.clone()).or_default() += 1;
                samples.push(s);
                if samples.len() >= want {
                    break 'outer;
                }
            }
        }
    }

    println!("chunks decoded: {chunks_read}");
    println!("\nsampled columns:");
    for s in &samples {
        println!(
            "  ({:>5},{:>5})  {:<34} surface {:>4} {}",
            s.x,
            s.z,
            s.biome,
            s.surface_y.map(|y| y.to_string()).unwrap_or("-".into()),
            s.surface_block.as_deref().unwrap_or("-")
        );
    }
    println!("\nbiome distribution over sampled columns:");
    for (b, n) in &biome_counts {
        println!("  {n:>4}  {b}");
    }
}

fn region_coords(path: &Path) -> (i32, i32) {
    let name = path.file_name().unwrap().to_str().unwrap();
    let parts: Vec<&str> = name.split('.').collect();
    (parts[1].parse().unwrap(), parts[2].parse().unwrap())
}

/// Pull one chunk's decompressed NBT out of a region file.
fn chunk_payload(data: &[u8], idx: usize) -> Option<Vec<u8>> {
    let e = idx * 4;
    let offset =
        ((data[e] as usize) << 16) | ((data[e + 1] as usize) << 8) | (data[e + 2] as usize);
    let sectors = data[e + 3] as usize;
    if offset == 0 || sectors == 0 {
        return None; // never generated
    }
    let start = offset * 4096;
    let len = u32::from_be_bytes(data.get(start..start + 4)?.try_into().ok()?) as usize;
    let scheme = *data.get(start + 4)?;
    let body = data.get(start + 5..start + 4 + len)?;
    match scheme {
        1 => None, // gzip: vanilla does not write it, so don't pretend to read it
        2 => miniz_oxide::inflate::decompress_to_vec_zlib(body).ok(),
        3 => Some(body.to_vec()),
        _ => None, // e.g. LZ4, which this tool does not handle
    }
}

/// Biome and surface of the column at the chunk's local (0, 0).
fn sample_column(root: &Nbt, cx: i32, cz: i32) -> Option<Sample> {
    let sections = compound_get(root, "sections")?;
    let Nbt::List(list) = sections else {
        return None;
    };

    let mut biome = None;
    let mut surface: Option<(i32, String)> = None;

    for sec in list {
        if std::env::var_os("ORACLE_DIAG").is_some() {
            if let Nbt::Compound(f) = sec {
                let keys: Vec<&str> = f.iter().map(|(k, _)| k.as_str()).collect();
                eprintln!("section keys: {keys:?}");
                if let Some(bs) = compound_get(sec, "block_states") {
                    if let Nbt::Compound(bf) = bs {
                        let bk: Vec<&str> = bf.iter().map(|(k, _)| k.as_str()).collect();
                        eprintln!("  block_states keys: {bk:?}");
                    } else {
                        eprintln!("  block_states is {bs:?}");
                    }
                }
            }
        }
        let y = match compound_get(sec, "Y") {
            Some(Nbt::Byte(b)) => *b as i32,
            _ => continue,
        };

        // Biomes: a 4×4×4 palette per section. The column's own quart is
        // index 0 when sampling local (0, 0).
        if biome.is_none() {
            if let Some(names) = palette_names(sec, "biomes") {
                if !names.is_empty() {
                    biome = Some(names[0].clone());
                }
            }
        }

        // Surface: highest section wins, so keep scanning upward.
        if let Some(names) = palette_names(sec, "block_states") {
            if names.len() == 1 {
                if names[0] != "minecraft:air" {
                    surface = Some((y * 16 + 15, names[0].clone()));
                }
            } else if names.iter().any(|n| n != "minecraft:air") {
                // Mixed section: report it without unpacking the bit-packed
                // indices — the parity comparison proper will need that, but
                // for a smoke test the section's presence is enough.
                surface = Some((y * 16, format!("{} (mixed)", names.len())));
            }
        }
    }

    Some(Sample {
        x: cx * 16,
        z: cz * 16,
        biome: biome?,
        surface_y: surface.as_ref().map(|(y, _)| *y),
        surface_block: surface.map(|(_, b)| b),
    })
}

/// The palette entry names of `field` inside a section.
fn palette_names(section: &Nbt, field: &str) -> Option<Vec<String>> {
    let container = compound_get(section, field)?;
    let Nbt::List(entries) = compound_get(container, "palette")? else {
        return None;
    };
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        match e {
            Nbt::String(s) => out.push(s.clone()),
            // Block states are compounds carrying a Name plus properties.
            Nbt::Compound(_) => match compound_get(e, "Name") {
                Some(Nbt::String(s)) => out.push(s.clone()),
                _ => return None,
            },
            _ => return None,
        }
    }
    Some(out)
}

fn compound_get<'a>(v: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    let Nbt::Compound(fields) = v else {
        return None;
    };
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}
