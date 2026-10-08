//! What the vanilla generator costs in memory: after loading, after the first
//! column, and after a square of them, with what its caches hold.
//!
//! ```text
//! cargo run --release -p aether-worldgen --example vanilla_memory -- <pack-root> [side]
//! ```
//!
//! Linux only for the resident-set figures (`/proc/self/statm`). Single
//! threaded: the server's workers add their own per-thread scratch on top.

use aether_worldgen::vanilla::generator::VanillaGenerator;
use aether_worldgen::ChunkGenerator;

fn rss_mib() -> String {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| format!("{} MiB", pages * 4096 / (1024 * 1024)))
        .unwrap_or_else(|| "?".into())
}

fn report(gen: &VanillaGenerator, what: &str) {
    let ((bn, bb), (dn, db)) = gen.cache_footprint();
    println!(
        "{what:<28} RSS {:>8}   carved chunks kept: {bn} ({} MiB)   decoration kept: {dn} ({} MiB)",
        rss_mib(),
        bb / (1024 * 1024),
        db / (1024 * 1024)
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(pack) = args.first() else {
        eprintln!("usage: vanilla_memory <pack-root> [side]");
        std::process::exit(2);
    };
    let side: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(24);
    println!("{:<28} RSS {:>8}", "start", rss_mib());
    let gen = VanillaGenerator::new(pack, 42).unwrap_or_else(|e| panic!("{e}"));
    report(&gen, "generator loaded");
    let _ = gen.generate_column(0, 0);
    report(&gen, "after the first column");
    for z in 0..side {
        for x in 0..side {
            let _ = gen.generate_column(x, z);
        }
    }
    report(&gen, &format!("after {side}x{side} columns"));
}
