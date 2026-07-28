//! `aether-convert` — CLI front-end for the Anvil → Aether KV migration.
//!
//! ```text
//! aether-convert <world-dir> <output-dir> [--threads N] [--mem]
//! ```
//!
//! * `<world-dir>`  — a world folder (containing `region/`) or a `region/` dir.
//! * `<output-dir>` — destination for the Fjall KV store (created if absent).
//! * `--threads N`  — worker threads (default: available parallelism).
//! * `--mem`        — dry run into an in-memory store (nothing persisted).

use aether_convert::convert::{convert_world, default_threads};
use aether_world::{MemStore, WorldStorage};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

fn usage() -> ExitCode {
    eprintln!(
        "usage: aether-convert <world-dir> <output-dir> [--threads N] [--mem]\n\
         \n  <world-dir>   world folder (with region/) or a region/ directory\
         \n  <output-dir>  destination KV store directory\
         \n  --threads N   worker threads (default: CPU parallelism)\
         \n  --mem         dry-run into memory (ignores <output-dir> persistence)"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut threads = default_threads();
    let mut mem = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--threads" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) if n >= 1 => threads = n,
                    _ => return usage(),
                }
            }
            "--mem" => mem = true,
            "-h" | "--help" => return usage(),
            other if other.starts_with("--") => {
                eprintln!("unknown flag: {other}");
                return usage();
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    if positional.len() != 2 {
        return usage();
    }
    let world_dir = PathBuf::from(&positional[0]);
    let output_dir = PathBuf::from(&positional[1]);

    if !world_dir.exists() {
        eprintln!("error: world dir `{}` does not exist", world_dir.display());
        return ExitCode::FAILURE;
    }

    let dest = if mem {
        "<memory>".to_string()
    } else {
        output_dir.display().to_string()
    };
    println!(
        "aether-convert: {} -> {} ({} thread{})",
        world_dir.display(),
        dest,
        threads,
        if threads == 1 { "" } else { "s" }
    );

    let start = Instant::now();
    let report = if mem {
        let storage = WorldStorage::new(MemStore::new());
        convert_world(&world_dir, &storage, threads)
    } else {
        match open_persistent(&output_dir) {
            Ok(storage) => convert_world(&world_dir, &storage, threads),
            Err(code) => return code,
        }
    };

    let report = match report {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let elapsed = start.elapsed();

    println!("\n── conversion report ──");
    println!("regions         : {}", report.regions);
    println!("chunks          : {}", report.chunks);
    println!("sub-chunks saved: {}", report.sections);
    println!("distinct blocks : {}", report.distinct_blocks);
    println!("audit checksum  : {:#018x}", report.checksum);
    println!("elapsed         : {:.2?}", elapsed);
    if report.sections > 0 {
        let cps = report.chunks as f64 / elapsed.as_secs_f64().max(1e-9);
        println!("throughput      : {cps:.0} chunks/sec");
    }
    let flush_failed = report.errors.iter().any(|e| e.starts_with("flush failed"));
    if !report.errors.is_empty() {
        println!("\n{} non-fatal error(s):", report.errors.len());
        for e in report.errors.iter().take(20) {
            println!("  - {e}");
        }
        if report.errors.len() > 20 {
            println!("  … and {} more", report.errors.len() - 20);
        }
    }

    // Per-chunk decode errors stay non-fatal, but a failed final flush means the
    // output may not be durable — fail so scripts/CI notice.
    if flush_failed {
        eprintln!("error: final flush failed; output may be incomplete");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(feature = "fjall")]
fn open_persistent(
    output_dir: &std::path::Path,
) -> Result<WorldStorage<aether_world::FjallStore>, ExitCode> {
    match aether_world::FjallStore::open(output_dir) {
        Ok(store) => Ok(WorldStorage::new(store)),
        Err(e) => {
            eprintln!("error: opening KV store `{}`: {e}", output_dir.display());
            Err(ExitCode::FAILURE)
        }
    }
}

#[cfg(not(feature = "fjall"))]
fn open_persistent(_output_dir: &std::path::Path) -> Result<WorldStorage<MemStore>, ExitCode> {
    eprintln!(
        "error: this build has no persistent backend (built with --no-default-features).\n\
         Re-run with the `fjall` feature, or pass --mem for a dry run."
    );
    Err(ExitCode::from(3))
}
