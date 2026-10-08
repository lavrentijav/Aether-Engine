//! Render a top-down map of generated terrain to a PPM image.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_map -- <pack-root> <seed> <x0> <z0> <size> <out.ppm> [threads]
//! ```
//!
//! Each pixel is the topmost non-air block of its column, coloured by block
//! and shaded by height (lit from the north-west), with water tinted by depth.
//! Also prints the generation time per column and the feature types the
//! decorator skipped.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use aether_world::registry::blocks;
use aether_world::BlockStateId;
use aether_worldgen::vanilla::generator::VanillaGenerator;
use aether_worldgen::ChunkGenerator;

fn color(name: &str) -> [u8; 3] {
    let n = name.strip_prefix("minecraft:").unwrap_or(name);
    match n {
        "grass_block" => [96, 158, 64],
        "short_grass" | "tall_grass" | "fern" | "large_fern" | "bush" => [84, 150, 58],
        "dirt" | "coarse_dirt" | "rooted_dirt" => [134, 96, 67],
        "podzol" => [106, 76, 40],
        "mycelium" => [111, 99, 105],
        "sand" => [219, 207, 163],
        "red_sand" => [190, 102, 33],
        "sandstone" => [216, 203, 155],
        "gravel" => [131, 127, 126],
        "stone" | "andesite" | "tuff" | "cobblestone" | "mossy_cobblestone" => [125, 125, 125],
        "granite" => [149, 103, 85],
        "diorite" => [188, 188, 188],
        "deepslate" => [80, 80, 82],
        "calcite" => [223, 224, 220],
        "water" => [52, 90, 210],
        "ice" | "packed_ice" | "blue_ice" => [160, 190, 250],
        "snow" | "snow_block" | "powder_snow" => [248, 250, 252],
        "lava" => [230, 90, 20],
        "clay" => [160, 166, 179],
        "mud" => [60, 57, 61],
        "terracotta" => [152, 94, 67],
        "white_terracotta" => [209, 178, 161],
        "orange_terracotta" => [161, 83, 37],
        "yellow_terracotta" => [186, 133, 35],
        "brown_terracotta" => [77, 51, 35],
        "red_terracotta" => [143, 61, 46],
        "light_gray_terracotta" => [135, 107, 98],
        "cactus" => [85, 127, 43],
        "sugar_cane" => [148, 192, 101],
        "bamboo" => [93, 144, 19],
        "lily_pad" => [32, 128, 48],
        "kelp" | "kelp_plant" | "seagrass" | "tall_seagrass" => [40, 110, 90],
        "moss_block" | "moss_carpet" => [89, 109, 45],
        "dead_bush" | "short_dry_grass" | "tall_dry_grass" => [148, 112, 60],
        "pumpkin" => [198, 118, 24],
        "melon" => [111, 145, 30],
        "brown_mushroom_block" => [149, 111, 81],
        "red_mushroom_block" => [200, 46, 45],
        "mushroom_stem" => [203, 196, 185],
        "bedrock" => [40, 40, 40],
        _ => {
            if n.ends_with("_leaves") {
                match n {
                    "birch_leaves" => [96, 140, 60],
                    "spruce_leaves" => [56, 90, 56],
                    "cherry_leaves" => [233, 178, 205],
                    "azalea_leaves" | "flowering_azalea_leaves" => [90, 120, 40],
                    "pale_oak_leaves" => [160, 168, 150],
                    _ => [48, 110, 30],
                }
            } else if n.ends_with("_log") || n.ends_with("_wood") {
                [104, 82, 50]
            } else if n.contains("tulip")
                || n.contains("poppy")
                || n.contains("dandelion")
                || n.contains("orchid")
                || n.contains("allium")
                || n.contains("bluet")
                || n.contains("daisy")
                || n.contains("cornflower")
                || n.contains("lily_of_the_valley")
                || n.contains("rose")
                || n.contains("lilac")
                || n.contains("peony")
                || n.contains("sunflower")
                || n.contains("petals")
                || n.contains("wildflowers")
            {
                [220, 80, 160]
            } else if n.contains("coral") {
                [230, 120, 180]
            } else if n.contains("ore") {
                [150, 150, 150]
            } else {
                let h = n.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
                [(h & 0xff) as u8, ((h >> 8) & 0xff) as u8, ((h >> 16) & 0xff) as u8]
            }
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 6 {
        eprintln!("usage: vanilla_map <pack-root> <seed> <x0> <z0> <size> <out.ppm> [threads]");
        std::process::exit(2);
    }
    let seed: i64 = a[1].parse().unwrap();
    let x0: i32 = a[2].parse().unwrap();
    let z0: i32 = a[3].parse().unwrap();
    let size: i32 = a[4].parse().unwrap();
    let threads: usize = a.get(6).and_then(|s| s.parse().ok()).unwrap_or(4);
    let t0 = Instant::now();
    let gen = VanillaGenerator::new(&a[0], seed as u64).unwrap_or_else(|e| panic!("{e}"));
    println!("load: {:?}", t0.elapsed());
    let (cx0, cz0) = (x0.div_euclid(16), z0.div_euclid(16));
    let (cx1, cz1) = ((x0 + size - 1).div_euclid(16), (z0 + size - 1).div_euclid(16));
    let chunks: Vec<(i32, i32)> = (cz0..=cz1).flat_map(|z| (cx0..=cx1).map(move |x| (x, z))).collect();
    let next = AtomicUsize::new(0);
    // Per pixel: (top block, its y, water depth above the floor).
    let mut pixels = vec![(BlockStateId::AIR, 0i32, 0i32); (size * size) as usize];
    let t = Instant::now();
    let results: Vec<Vec<(i32, i32, BlockStateId, i32, i32)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(cx, cz)) = chunks.get(i) else { break };
                        let col = gen.generate_column(cx, cz);
                        let get = |lx: usize, y: i32, lz: usize| -> BlockStateId {
                            let cy = y.div_euclid(16) as i8;
                            col.sections
                                .iter()
                                .find(|(c, _)| *c == cy)
                                .map(|(_, sc)| sc.get(lx, y.rem_euclid(16) as usize, lz))
                                .unwrap_or(BlockStateId::AIR)
                        };
                        let name = |id: BlockStateId| blocks::block_of_state(id).map(|b| b.1).unwrap_or("minecraft:air");
                        let top_y = col.sections.last().map(|(c, _)| *c as i32 * 16 + 15).unwrap_or(-64);
                        for lz in 0..16usize {
                            for lx in 0..16usize {
                                let mut y = top_y;
                                while y > -64
                                    && matches!(name(get(lx, y, lz)), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
                                {
                                    y -= 1;
                                }
                                let id = get(lx, y, lz);
                                let mut depth = 0;
                                if name(id) == "minecraft:water" {
                                    let mut d = y;
                                    while d > -64 && name(get(lx, d, lz)) == "minecraft:water" {
                                        d -= 1;
                                        depth += 1;
                                    }
                                }
                                out.push((cx * 16 + lx as i32, cz * 16 + lz as i32, id, y, depth));
                            }
                        }
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let elapsed = t.elapsed();
    println!(
        "{} columns in {:?} ({:?} per column, {threads} threads)",
        chunks.len(),
        elapsed,
        elapsed * threads as u32 / chunks.len() as u32
    );
    for r in results {
        for (x, z, id, y, depth) in r {
            let (px, pz) = (x - x0, z - z0);
            if px >= 0 && pz >= 0 && px < size && pz < size {
                pixels[(pz * size + px) as usize] = (id, y, depth);
            }
        }
    }
    let mut img = Vec::with_capacity((size * size * 3) as usize);
    for pz in 0..size {
        for px in 0..size {
            let (id, y, depth) = pixels[(pz * size + px) as usize];
            let name = blocks::block_of_state(id).map(|b| b.1).unwrap_or("minecraft:air");
            let mut c = color(name);
            let shade = if px > 0 && pz > 0 {
                let (_, yn, _) = pixels[((pz - 1) * size + px - 1) as usize];
                (y - yn).clamp(-4, 4) as f32 * 0.06
            } else {
                0.0
            };
            let mut f = 1.0 + shade;
            if name == "minecraft:water" {
                f *= (1.0 - depth as f32 * 0.03).max(0.45);
            }
            for v in &mut c {
                *v = (*v as f32 * f).clamp(0.0, 255.0) as u8;
            }
            img.extend_from_slice(&c);
        }
    }
    let mut file = format!("P6\n{size} {size}\n255\n").into_bytes();
    file.extend_from_slice(&img);
    std::fs::write(&a[5], file).expect("write image");
    println!("wrote {}", a[5]);
    let u = gen.unsupported_features();
    if !u.is_empty() {
        println!("skipped: {}", u.join(", "));
    }
}
