//! `aether` — a small, self-contained demo that proves the engine actually runs.
//!
//! It wires the shipped subsystems together end to end:
//!
//! 1. **SIMD dispatch** — reports the mask-op backend chosen for this CPU.
//! 2. **World generation** — generates terrain and prints a vertical
//!    cross-section so you can *see* the world.
//! 3. **Physics** — drops a player from the sky and ticks gravity + collision
//!    until it lands on the generated ground.
//! 4. **Lighting** — runs the flood-fill light engine over the centre column
//!    and reports sky light at the surface plus a placed glowstone's block light.
//! 5. **Storage** — persists the touched sub-chunks (in-memory or Fjall+Zstd).
//! 6. **Telemetry** — emits a Prometheus exposition of what happened.
//!
//! Run it with `cargo run -p aether-demo` (writes/reads `aether.toml`).
//!
//! `cargo run -p aether-demo -- stream` instead streams the world outward from
//! the centre chunk, generating and persisting it ring by ring until the
//! configured `[stream] radius` is reached or you press **Ctrl+C** — on which it
//! flushes everything generated to storage and exits ("Map saved").

mod config;

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use aether_api::{block_ids, Body, LightView, World, MAX_LIGHT};
use aether_api::{FlatGenerator, MemStore, NoiseGenerator};
use aether_core::Backend;
use aether_telemetry::{span, Registry};
use aether_world::{BlockStateId, KvBackend};
use aether_worldgen::ChunkGenerator;

use config::{Config, GeneratorKind, StorageKind};

/// Cleared by the SIGINT handler so the streaming loop can flush and exit.
static RUNNING: AtomicBool = AtomicBool::new(true);

/// Which mode the binary runs in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One-shot: generate, drop a player, tick, report.
    Once,
    /// Stream chunks outward forever (until Ctrl+C), saving as it goes.
    Stream,
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let mut mode = Mode::Once;
    if args.peek().map(String::as_str) == Some("stream") {
        mode = Mode::Stream;
        args.next();
    }
    let arg = args.next();
    if matches!(arg.as_deref(), Some("-h") | Some("--help")) {
        eprintln!("usage: aether [stream] [config.toml]   (default config: aether.toml)");
        eprintln!("  stream  generate the world outward and save on Ctrl+C");
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
        StorageKind::Memory => with_generator(MemStore::new(), &cfg, mode),
        StorageKind::Fjall => open_persistent(&cfg, mode),
    }
}

/// Install the SIGINT (Ctrl+C) handler that stops the streaming loop.
#[cfg(unix)]
fn install_sigint() {
    extern "C" fn on_sigint(_sig: libc::c_int) {
        RUNNING.store(false, Ordering::SeqCst);
    }
    // Coerce to a function pointer first, then to the handler integer type
    // (a direct fn-item -> integer cast is a clippy error).
    let handler: extern "C" fn(libc::c_int) = on_sigint;
    // SAFETY: `on_sigint` only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
    }
}

#[cfg(not(unix))]
fn install_sigint() {}

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
fn open_persistent(cfg: &Config, mode: Mode) -> ExitCode {
    match aether_api::FjallStore::open(&cfg.world.storage_path) {
        Ok(store) => {
            println!("storage    : Fjall store at `{}`\n", cfg.world.storage_path);
            with_generator(store, cfg, mode)
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
fn open_persistent(cfg: &Config, mode: Mode) -> ExitCode {
    eprintln!("note: this build has no `persist` feature; falling back to the in-memory store.\n");
    with_generator(MemStore::new(), cfg, mode)
}

fn with_generator<B: KvBackend>(backend: B, cfg: &Config, mode: Mode) -> ExitCode {
    match cfg.world.generator {
        GeneratorKind::Flat => dispatch(World::new(backend, FlatGenerator::classic()), cfg, mode),
        GeneratorKind::Noise => dispatch(
            World::new(backend, NoiseGenerator::new(cfg.world.seed)),
            cfg,
            mode,
        ),
    }
}

fn dispatch<B: KvBackend, G: ChunkGenerator>(
    world: World<B, G>,
    cfg: &Config,
    mode: Mode,
) -> ExitCode {
    match mode {
        Mode::Once => run(world, cfg),
        Mode::Stream => run_stream(world, cfg),
    }
}

/// Chunk coordinates on the square ring at Chebyshev distance `r` from `centre`.
fn ring(centre: (i32, i32), r: i32) -> Vec<(i32, i32)> {
    let (cx, cz) = centre;
    if r == 0 {
        return vec![(cx, cz)];
    }
    let mut out = Vec::new();
    for x in (cx - r)..=(cx + r) {
        out.push((x, cz - r));
        out.push((x, cz + r));
    }
    for z in (cz - r + 1)..=(cz + r - 1) {
        out.push((cx - r, z));
        out.push((cx + r, z));
    }
    out
}

/// Streaming mode: generate the world outward from the centre ring by ring,
/// persisting as it goes, until the configured radius or a Ctrl+C. On interrupt
/// (or completion) the world is flushed so nothing generated is lost.
fn run_stream<B: KvBackend, G: ChunkGenerator>(world: World<B, G>, cfg: &Config) -> ExitCode {
    install_sigint();

    let reg = Registry::new();
    let generated = reg.counter("aether_stream_chunks_generated_total");

    let centre = (
        cfg.demo.center_x.div_euclid(16),
        cfg.demo.center_z.div_euclid(16),
    );
    let max_r = cfg.stream.radius;
    let flush_every = cfg.stream.flush_every.max(1) as u64;

    println!("── streaming world generation ──");
    println!("centre chunk : {centre:?}");
    if max_r > 0 {
        println!("radius       : {max_r} rings");
    } else {
        println!("radius       : unbounded (until Ctrl+C)");
    }
    println!("Press Ctrl+C to save and exit.\n");

    if cfg.world.storage == StorageKind::Memory {
        eprintln!(
            "note: storage = memory — chunks are generated but not written to disk;\n      set storage = \"fjall\" to persist across restarts.\n"
        );
    }

    let mut count: u64 = 0;
    let mut interrupted = false;
    let mut r = 0i32;
    'rings: loop {
        if max_r > 0 && r > max_r {
            break;
        }
        for (ccx, ccz) in ring(centre, r) {
            if !RUNNING.load(Ordering::SeqCst) {
                interrupted = true;
                break 'rings;
            }
            // Touching any block forces the whole column to load/generate (and
            // the World persists freshly generated sections to storage).
            let _ = world.get_block(ccx * 16 + 8, 64, ccz * 16 + 8);
            count += 1;
            generated.inc();
            if count % flush_every == 0 {
                if let Err(e) = world.flush() {
                    eprintln!("error: flushing world storage: {e}");
                    return ExitCode::FAILURE;
                }
                println!(
                    "  generated {count} chunks (ring {r}, resident sections {}) — saved",
                    world.resident_sections()
                );
            }
        }
        r += 1;
    }

    // Final durable flush so nothing is lost on exit.
    if let Err(e) = world.flush() {
        eprintln!("error: final flush: {e}");
        return ExitCode::FAILURE;
    }

    println!();
    if interrupted {
        println!("interrupted — flushed {count} generated chunks to storage. Map saved.");
    } else {
        println!("done — generated and saved {count} chunks.");
    }
    ExitCode::SUCCESS
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

    // --- 3. Lighting: compute real block + sky light for the centre column. ---
    let surface_sky = reg.gauge("aether_demo_surface_sky_light");
    let torch_block = reg.gauge("aether_demo_glowstone_block_light");
    // Drop a glowstone a few blocks above the surface and off to one side so it
    // has room to radiate without shadowing the centre sky column, then light
    // the whole column. Keep it inside the same chunk footprint (lx 0..16).
    let glow_x = (cx & !15) + ((cx & 15) + 4).min(15);
    let glow_y = ground + 3;
    world.set_block(glow_x, glow_y, cz, "minecraft:glowstone");
    let light = world.light_column(cx.div_euclid(16), cz.div_euclid(16));
    let sky_above = light.sky_light(cx, ground + 1, cz);
    let sky_below = light.sky_light(cx, ground.saturating_sub(2), cz);
    let glow_here = light.block_light(glow_x, glow_y, cz);
    let glow_near = light.block_light(glow_x + 1, glow_y, cz);
    surface_sky.set(sky_above as i64);
    torch_block.set(glow_here as i64);

    println!("── lighting (flood-fill) ──");
    println!(
        "centre column       : ({}, {})",
        cx.div_euclid(16),
        cz.div_euclid(16)
    );
    println!("sky light  above/below surface : {sky_above}/{sky_below}  (max {MAX_LIGHT})");
    println!("block light at glowstone / +1x : {glow_here}/{glow_near}");
    println!();

    // --- 4. Storage: persist everything the run touched. ---
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
    println!(
        "  [{}] lighting computed (surface sky={sky_above}, glowstone block={glow_here})",
        check(sky_above == MAX_LIGHT && glow_here == MAX_LIGHT)
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
