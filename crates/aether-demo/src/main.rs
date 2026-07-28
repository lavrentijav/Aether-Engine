//! `aether` — a small, self-contained demo that proves the engine actually runs.
//!
//! It wires the shipped subsystems together end to end:
//!
//! 1. **SIMD dispatch** — reports the mask-op backend chosen for this CPU.
//! 2. **World generation** — generates terrain and prints a vertical
//!    cross-section so you can *see* the world.
//! 3. **Physics** — drops a player from the sky and ticks gravity + collision
//!    until it lands on the generated ground.
//! 4. **Storage** — persists the touched sub-chunks (in-memory or Fjall+Zstd).
//! 5. **Telemetry** — emits a Prometheus exposition of what happened.
//!
//! Run it with `cargo run -p aether-demo` (writes/reads `aether.toml`).

mod config;

use std::process::ExitCode;

use aether_api::{block_ids, Body, World};
use aether_api::{FlatGenerator, MemStore, NoiseGenerator};
use aether_core::Backend;
use aether_telemetry::{span, Registry};
use aether_world::{BlockStateId, KvBackend};
use aether_worldgen::ChunkGenerator;

use config::{Config, GeneratorKind, StorageKind};

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("-h") | Some("--help")) {
        eprintln!("usage: aether [config.toml]   (default: aether.toml)");
        return ExitCode::SUCCESS;
    }
    let path = arg.unwrap_or_else(|| "aether.toml".to_string());

    let (cfg, created) = match Config::load_or_init(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if created {
        println!("No config at `{path}` — wrote a sample there; using defaults for this run.\n");
    }

    print_banner(&cfg, &path);

    match cfg.world.storage {
        StorageKind::Memory => with_generator(MemStore::new(), &cfg),
        StorageKind::Fjall => open_persistent(&cfg),
    }
}

fn print_banner(cfg: &Config, path: &str) {
    let backend = Backend::detect();
    println!("╔══════════════════════════════════════════════╗");
    println!("║             Aether Engine — demo             ║");
    println!("╚══════════════════════════════════════════════╝");
    println!("config     : {path}");
    println!("simd path  : {} (mask ops)", backend.name());
    println!(
        "world      : generator={:?}, storage={:?}, seed={}",
        cfg.world.generator, cfg.world.storage, cfg.world.seed
    );
    println!();
}

#[cfg(feature = "persist")]
fn open_persistent(cfg: &Config) -> ExitCode {
    match aether_api::FjallStore::open(&cfg.world.storage_path) {
        Ok(store) => {
            println!("storage    : Fjall store at `{}`\n", cfg.world.storage_path);
            with_generator(store, cfg)
        }
        Err(e) => {
            eprintln!(
                "error: opening Fjall store `{}`: {e}",
                cfg.world.storage_path
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "persist"))]
fn open_persistent(cfg: &Config) -> ExitCode {
    eprintln!("note: this build has no `persist` feature; falling back to the in-memory store.\n");
    with_generator(MemStore::new(), cfg)
}

fn with_generator<B: KvBackend>(backend: B, cfg: &Config) -> ExitCode {
    match cfg.world.generator {
        GeneratorKind::Flat => run(World::new(backend, FlatGenerator::classic()), cfg),
        GeneratorKind::Noise => run(
            World::new(backend, NoiseGenerator::new(cfg.world.seed)),
            cfg,
        ),
    }
}

/// Map a block id to a single display glyph for the cross-section.
fn glyph(id: BlockStateId) -> char {
    use block_ids as b;
    if id == b::AIR {
        ' '
    } else if id == b::GRASS_BLOCK {
        '#'
    } else if id == b::DIRT {
        '+'
    } else if id == b::STONE {
        '.'
    } else if id == b::SAND {
        ':'
    } else if id == b::GRAVEL {
        ';'
    } else if id == b::WATER {
        '~'
    } else if id == b::BEDROCK {
        '_'
    } else if id == b::OAK_LOG {
        '|'
    } else if id == b::OAK_LEAVES {
        '*'
    } else {
        'X'
    }
}

fn run<B: KvBackend, G: ChunkGenerator>(world: World<B, G>, cfg: &Config) -> ExitCode {
    let reg = Registry::new();
    let ticks_total = reg.counter("aether_demo_physics_ticks_total");
    let blocks_queried = reg.counter("aether_demo_blocks_queried_total");
    let worldgen_ns = reg.counter("aether_demo_worldgen_ns");
    let sections = reg.gauge("aether_demo_resident_sections");

    // Clamp the view so a stray config can't flood the terminal.
    let radius = cfg.demo.view_radius.clamp(4, 120);
    let (cx, cz) = (cfg.demo.center_x, cfg.demo.center_z);
    let (x0, x1) = (cx - radius, cx + radius);

    // --- 1. World generation: sample surface heights across the row. ---
    let mut surfaces = Vec::with_capacity((x1 - x0 + 1) as usize);
    {
        let _t = span!(worldgen_ns);
        for x in x0..=x1 {
            surfaces.push(world.height_hint(x, cz));
        }
    }
    let max_s = *surfaces.iter().max().unwrap_or(&64);
    let min_s = *surfaces.iter().min().unwrap_or(&0);
    let top = max_s + 2;
    let bottom = (min_s - 6).max(0);

    println!("── world cross-section  (z = {cz}, x = {x0}..={x1}) ──");
    for y in (bottom..=top).rev() {
        let mut line = String::with_capacity((x1 - x0 + 1) as usize);
        for x in x0..=x1 {
            let id = world.get_block(x, y, cz);
            blocks_queried.inc();
            line.push(glyph(id));
        }
        println!("{y:4} |{line}");
    }
    println!(
        "     legend: '#'=grass '+'=dirt '.'=stone ':'=sand ';'=gravel '~'=water '_'=bedrock\n"
    );

    // --- 2. Physics: drop a player and let gravity + collision settle it. ---
    let (sx, sz) = (cx as f64 + 0.5, cz as f64 + 0.5);
    let ground = world.height_hint(cx, cz);
    let mut body: Body = world.spawn_player(sx, cfg.demo.spawn_height, sz);
    let mut landed_at: Option<u32> = None;
    for t in 0..cfg.demo.ticks {
        world.step_body(&mut body);
        ticks_total.inc();
        if body.on_ground && landed_at.is_none() {
            landed_at = Some(t + 1);
        }
    }
    let feet = body.feet();

    println!("── physics: player drop ──");
    println!(
        "spawn       : ({sx:.1}, {:.1}, {sz:.1})  above surface y≈{ground}",
        cfg.demo.spawn_height
    );
    match landed_at {
        Some(t) => println!("landed      : after {t} tick(s)"),
        None => println!("landed      : still falling after {} ticks", cfg.demo.ticks),
    }
    println!(
        "final feet  : ({:.2}, {:.3}, {:.2})   on_ground={}",
        feet.x, feet.y, feet.z, body.on_ground
    );
    println!();

    // --- 3. Storage: persist everything the run touched. ---
    if let Err(e) = world.flush() {
        eprintln!("error: flushing world storage: {e}");
        return ExitCode::FAILURE;
    }
    sections.set(world.resident_sections() as i64);
    println!("── storage ──");
    println!("resident sub-chunks : {}", world.resident_sections());
    println!("flush               : ok\n");

    // A concise "does it work?" checklist.
    let landed_ok = landed_at.is_some() && body.on_ground;
    println!("── result ──");
    println!("  [{}] SIMD dispatch selected a backend", check(true));
    println!(
        "  [{}] world generated & rendered",
        check(!surfaces.is_empty())
    );
    println!("  [{}] player fell and landed on ground", check(landed_ok));
    println!("  [{}] world storage flushed", check(true));
    println!();

    // --- 4. Telemetry: Prometheus exposition. ---
    if cfg.telemetry.prometheus {
        println!("── telemetry (Prometheus) ──");
        print!("{}", reg.render_prometheus());
    }

    if landed_ok {
        ExitCode::SUCCESS
    } else {
        // Not necessarily a failure (a flat void could leave nothing to land on),
        // but signal it so scripted checks can notice.
        ExitCode::from(1)
    }
}

fn check(ok: bool) -> char {
    if ok {
        '✔'
    } else {
        '✗'
    }
}
