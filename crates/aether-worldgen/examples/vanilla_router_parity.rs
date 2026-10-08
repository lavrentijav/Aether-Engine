//! Compare every density function in the overworld noise router, value by
//! value, against the game's own evaluation of the same graph.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_router_parity -- \
//!     <pack-root> <seed> <dump-file>
//! ```
//!
//! # Where the dump comes from
//!
//! `<dump-file>` is produced by a tiny Java program that links against the
//! operator's own server jar and asks *vanilla* for the numbers — it is not a
//! second Rust implementation, and not a transcription. One line per sampled
//! position:
//!
//! ```text
//! <x> <y> <z> <continents> <erosion> <ridges> <depth> <temperature>
//!             <vegetation> <final_density> <barrier> <lava> <vein_toggle>
//!             <vein_ridged> <vein_gap> <preliminary_surface_level>
//!             <fluid_level_floodedness> <fluid_level_spread>
//! ```
//!
//! with every value written as the raw `double` bit pattern in hex, so the
//! comparison is exact rather than "close enough":
//!
//! ```java
//! net.minecraft.SharedConstants.tryDetectVersion();
//! net.minecraft.server.Bootstrap.bootStrap();
//! HolderLookup.Provider p = VanillaRegistries.createLookup();
//! RandomState rs = RandomState.create(p, NoiseGeneratorSettings.OVERWORLD, seed);
//! NoiseRouter r = rs.router();
//! // ... print Long.toHexString(Double.doubleToRawLongBits(f.compute(ctx)))
//! ```
//!
//! Nothing from the game is checked into this repository — the dump is
//! regenerated from the operator's copy when it is needed.

#![allow(clippy::type_complexity)]

use std::sync::Arc;

use aether_worldgen::vanilla::density::{Builder, Ctx, DataPack, Node, NoiseRegistry};

/// Router entries, in the order the dump writes them.
const ENTRIES: [&str; 15] = [
    "continents",
    "erosion",
    "ridges",
    "depth",
    "temperature",
    "vegetation",
    "final_density",
    "barrier",
    "lava",
    "vein_toggle",
    "vein_ridged",
    "vein_gap",
    "preliminary_surface_level",
    "fluid_level_floodedness",
    "fluid_level_spread",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: vanilla_router_parity <pack-root> <seed> <dump-file>");
        std::process::exit(2);
    }
    let seed: i64 = args[1].parse().expect("seed must be an integer");

    let pack = DataPack::open(&args[0]).unwrap_or_else(|e| panic!("{e}"));
    let noises = NoiseRegistry::new(pack.clone(), seed as u64);
    let settings = pack
        .noise_settings("minecraft:overworld")
        .unwrap_or_else(|e| panic!("{e}"));
    let router = settings.get("noise_router").expect("no noise_router");

    let mut builder = Builder::new(&pack, &noises);
    // A function this crate cannot build yet is reported as such rather than
    // quietly dropped, so the sample size per entry stays honest.
    let mut built: Vec<Option<Arc<Node>>> = Vec::new();
    for name in ENTRIES {
        let json = router
            .get(name)
            .unwrap_or_else(|| panic!("no router entry `{name}`"));
        match builder.build(json) {
            Ok(f) => built.push(Some(f)),
            Err(e) => {
                println!("NOT BUILT  {name}: {e}");
                built.push(None);
            }
        }
    }

    let dump = std::fs::read_to_string(&args[2]).unwrap_or_else(|e| panic!("{}: {e}", args[2]));
    let mut agree = [0usize; ENTRIES.len()];
    let mut total = [0usize; ENTRIES.len()];
    let mut first_bad: Vec<Option<(i32, i32, i32, u64, u64)>> = vec![None; ENTRIES.len()];

    for line in dump.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 3 + ENTRIES.len() {
            continue;
        }
        let (x, y, z) = (
            f[0].parse::<i32>().unwrap(),
            f[1].parse::<i32>().unwrap(),
            f[2].parse::<i32>().unwrap(),
        );
        let ctx = Ctx::new(x, y, z);
        for (i, node) in built.iter().enumerate() {
            let Some(node) = node else { continue };
            let want = u64::from_str_radix(f[3 + i], 16).unwrap();
            let got = node.compute(ctx).to_bits();
            total[i] += 1;
            if got == want {
                agree[i] += 1;
            } else if first_bad[i].is_none() {
                first_bad[i] = Some((x, y, z, want, got));
            }
        }
    }

    println!("\nrouter entry                  agree / sampled");
    let mut all = (0usize, 0usize);
    for (i, name) in ENTRIES.iter().enumerate() {
        if total[i] == 0 {
            continue;
        }
        all.0 += agree[i];
        all.1 += total[i];
        println!("  {name:<28}{:>7} / {:<7}", agree[i], total[i]);
        if let Some((x, y, z, want, got)) = first_bad[i] {
            println!(
                "      first divergence at ({x},{y},{z}): vanilla {:e}, ours {:e}",
                f64::from_bits(want),
                f64::from_bits(got)
            );
        }
    }
    println!(
        "\ntotal: {}/{} values bit-identical to vanilla",
        all.0, all.1
    );
}
