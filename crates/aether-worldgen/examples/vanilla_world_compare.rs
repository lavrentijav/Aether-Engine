//! Compare the generator's finished chunks against a world a vanilla server
//! generated with the same seed, and render both as top-down maps.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_world_compare -- <pack-root> <seed> <region-dir> [vanilla.ppm ours.ppm]
//! ```
//!
//! Only chunks the server saved as `minecraft:full` are compared. Reported:
//! the share of all blocks that match, of columns whose top block matches,
//! and the most common disagreements. Structures (villages, …) are not
//! generated here, and feature overlap across chunk borders depends on the
//! order chunks were generated in even in vanilla, so 100% is not expected
//! above ground; below ground it should be close.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aether_convert::nbt::{self, Nbt};
use aether_world::registry::props;
use aether_world::BlockStateId;
use aether_worldgen::vanilla::blockinfo;
use aether_worldgen::vanilla::generator::{Stage, VanillaGenerator};

fn compound_get<'a>(v: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    let Nbt::Compound(fields) = v else { return None };
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn chunk_payload(data: &[u8], idx: usize) -> Option<Vec<u8>> {
    let e = idx * 4;
    let offset = ((data[e] as usize) << 16) | ((data[e + 1] as usize) << 8) | (data[e + 2] as usize);
    if offset == 0 || data[e + 3] == 0 {
        return None;
    }
    let start = offset * 4096;
    let len = u32::from_be_bytes(data.get(start..start + 4)?.try_into().ok()?) as usize;
    let body = data.get(start + 5..start + 4 + len)?;
    match data[start + 4] {
        2 => miniz_oxide::inflate::decompress_to_vec_zlib(body).ok(),
        3 => Some(body.to_vec()),
        _ => None,
    }
}

fn state_of(entry: &Nbt) -> BlockStateId {
    let Some(Nbt::String(name)) = compound_get(entry, "Name") else {
        return BlockStateId::AIR;
    };
    let mut s = name.clone();
    if let Some(Nbt::Compound(p)) = compound_get(entry, "Properties") {
        let parts: Vec<String> = p
            .iter()
            .filter_map(|(k, v)| if let Nbt::String(v) = v { Some(format!("{k}={v}")) } else { None })
            .collect();
        s = format!("{name}[{}]", parts.join(","));
    }
    blockinfo::parse_state(&s).unwrap_or(BlockStateId::AIR)
}

/// The chunk's blocks, `((y + 64) * 16 + z) * 16 + x`.
fn decode(root: &Nbt) -> Option<Vec<BlockStateId>> {
    let Nbt::List(list) = compound_get(root, "sections")? else { return None };
    let mut out = vec![BlockStateId::AIR; 384 * 256];
    for sec in list {
        let Some(Nbt::Byte(cy)) = compound_get(sec, "Y") else { continue };
        let cy = *cy as i32;
        if !(-4..20).contains(&cy) {
            continue;
        }
        let Some(bs) = compound_get(sec, "block_states") else { continue };
        let Some(Nbt::List(pal)) = compound_get(bs, "palette") else { continue };
        let palette: Vec<BlockStateId> = pal.iter().map(state_of).collect();
        let base = ((cy + 4) * 16) as usize * 256;
        match compound_get(bs, "data") {
            None => {
                for i in 0..4096 {
                    out[base + i] = palette[0];
                }
            }
            Some(Nbt::LongArray(longs)) => {
                let bits = (usize::BITS - (palette.len() - 1).leading_zeros()).max(4) as usize;
                let per = 64 / bits;
                let mask = (1u64 << bits) - 1;
                for i in 0..4096 {
                    let l = longs[i / per] as u64;
                    let v = ((l >> ((i % per) * bits)) & mask) as usize;
                    out[base + i] = palette.get(v).copied().unwrap_or(BlockStateId::AIR);
                }
            }
            _ => {}
        }
    }
    Some(out)
}

fn top(blocks: &[BlockStateId], x: usize, z: usize) -> (BlockStateId, i32) {
    for y in (0..384).rev() {
        let s = blocks[(y * 16 + z) * 16 + x];
        if !blockinfo::is_air(s) {
            return (s, y as i32 - 64);
        }
    }
    (BlockStateId::AIR, -64)
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("usage: vanilla_world_compare <pack-root> <seed> <region-dir> [vanilla.ppm ours.ppm]");
        std::process::exit(2);
    }
    let seed: i64 = a[1].parse().unwrap();
    let gen = VanillaGenerator::new(&a[0], seed as u64).unwrap_or_else(|e| panic!("{e}"));
    let dir = PathBuf::from(&a[2]);
    let mut chunks: BTreeMap<(i32, i32), Vec<BlockStateId>> = BTreeMap::new();
    for e in std::fs::read_dir(&dir).expect("region dir") {
        let p = e.unwrap().path();
        if p.extension().is_none_or(|x| x != "mca") {
            continue;
        }
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        let parts: Vec<&str> = name.split('.').collect();
        let (rx, rz): (i32, i32) = (parts[1].parse().unwrap(), parts[2].parse().unwrap());
        let data = std::fs::read(&p).unwrap();
        for idx in 0..1024 {
            let Some(payload) = chunk_payload(&data, idx) else { continue };
            let Ok(root) = nbt::parse(&payload) else { continue };
            match compound_get(&root, "Status") {
                Some(Nbt::String(s)) if s == "minecraft:full" => {}
                _ => continue,
            }
            if let Some(b) = decode(&root) {
                chunks.insert((rx * 32 + (idx % 32) as i32, rz * 32 + (idx / 32) as i32), b);
            }
        }
    }
    println!("full chunks: {}", chunks.len());
    let only_carvers = std::env::var_os("STAGE_CARVERS").is_some();
    let (mut same, mut total, mut top_same, mut cols) = (0usize, 0usize, 0usize, 0usize);
    let (mut below_same, mut below_total) = (0usize, 0usize);
    let mut diffs: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut ours: BTreeMap<(i32, i32), Vec<BlockStateId>> = BTreeMap::new();
    let t = std::time::Instant::now();
    for (&(cx, cz), want) in &chunks {
        let got = gen.chunk_at_stage(cx, cz, if only_carvers { Stage::Carvers } else { Stage::Features });
        for i in 0..want.len() {
            total += 1;
            let y = (i / 256) as i32 - 64;
            if y < 0 {
                below_total += 1;
            }
            if want[i] == got[i] {
                same += 1;
                if y < 0 {
                    below_same += 1;
                }
            } else {
                let k = (
                    blockinfo::name(want[i]).trim_start_matches("minecraft:").to_string(),
                    blockinfo::name(got[i]).trim_start_matches("minecraft:").to_string(),
                );
                *diffs.entry(k).or_default() += 1;
            }
        }
        for z in 0..16 {
            for x in 0..16 {
                cols += 1;
                if top(want, x, z) == top(&got, x, z) {
                    top_same += 1;
                }
            }
        }
        ours.insert((cx, cz), got);
    }
    println!("generated in {:?}", t.elapsed());
    let pct = |a: usize, b: usize| a as f64 * 100.0 / b.max(1) as f64;
    println!("blocks:        {same} / {total} ({:.3}%)", pct(same, total));
    println!("below y=0:     {below_same} / {below_total} ({:.3}%)", pct(below_same, below_total));
    println!("top block:     {top_same} / {cols} ({:.2}%)", pct(top_same, cols));
    let mut d: Vec<_> = diffs.into_iter().collect();
    d.sort_by(|a, b| b.1.cmp(&a.1));
    println!("most common differences (vanilla -> ours):");
    for ((w, g), n) in d.into_iter().take(25) {
        println!("  {n:>7}  {w} -> {g}");
    }
    if a.len() >= 5 {
        for (path, src) in [(&a[3], &chunks), (&a[4], &ours)] {
            render(Path::new(path), src);
        }
    }
    let _ = props::state_name(0);
}

fn render(path: &Path, chunks: &BTreeMap<(i32, i32), Vec<BlockStateId>>) {
    let (mut x0, mut z0, mut x1, mut z1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for &(cx, cz) in chunks.keys() {
        x0 = x0.min(cx);
        z0 = z0.min(cz);
        x1 = x1.max(cx);
        z1 = z1.max(cz);
    }
    let w = ((x1 - x0 + 1) * 16) as usize;
    let h = ((z1 - z0 + 1) * 16) as usize;
    let mut img = vec![0u8; w * h * 3];
    for (&(cx, cz), b) in chunks {
        for z in 0..16 {
            for x in 0..16 {
                let (s, y) = top(b, x, z);
                let n = blockinfo::name(s);
                let c = color(n, y);
                let px = (cx - x0) as usize * 16 + x;
                let pz = (cz - z0) as usize * 16 + z;
                img[(pz * w + px) * 3..(pz * w + px) * 3 + 3].copy_from_slice(&c);
            }
        }
    }
    let mut f = format!("P6\n{w} {h}\n255\n").into_bytes();
    f.extend_from_slice(&img);
    std::fs::write(path, f).unwrap();
}

fn color(n: &str, y: i32) -> [u8; 3] {
    let n = n.trim_start_matches("minecraft:");
    let base: [u8; 3] = if n.ends_with("_leaves") {
        [48, 110, 30]
    } else if n.ends_with("_log") {
        [104, 82, 50]
    } else {
        match n {
            "grass_block" => [96, 158, 64],
            "water" => [52, 90, 210],
            "sand" => [219, 207, 163],
            "stone" => [125, 125, 125],
            "dirt" | "coarse_dirt" => [134, 96, 67],
            "gravel" => [131, 127, 126],
            "snow" | "snow_block" => [248, 250, 252],
            "short_grass" | "tall_grass" | "fern" | "large_fern" => [84, 150, 58],
            "red_mushroom_block" => [200, 46, 45],
            "brown_mushroom_block" => [149, 111, 81],
            _ => {
                let h = n.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
                [(h & 0xff) as u8, ((h >> 8) & 0xff) as u8, ((h >> 16) & 0xff) as u8]
            }
        }
    };
    let f = 0.8 + ((y - 50).clamp(0, 60) as f32) / 150.0;
    base.map(|v| (v as f32 * f).min(255.0) as u8)
}
