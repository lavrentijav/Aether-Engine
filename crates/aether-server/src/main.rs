//! `aether-server` — a multi-version Minecraft server front end for the
//! engine.
//!
//! Scope is deliberately small — offline mode, no compression, no encryption,
//! creative game mode, and **light pinned to maximum**
//! ([`aether_world::FullBright`]). It exercises the real engine world for
//! chunk data, assigns each connection an [`aether_api::Player`] entity, and
//! keeps a [`players::Registry`] so connections can see each other.
//!
//! Protocol support is a translation layer rather than a fork per version:
//! the engine's own state is the anchor, and each supported client version is
//! a codec in [`protocol`] that renders that state into its own packets. See
//! [`protocol`] for the neutral vocabulary and [`session`] for the per-client
//! logic every version shares.

mod commands;
mod db;
mod economy;
mod game;
mod gencache;
mod ground;
mod inventory;
mod config;
mod placement;
mod players;
mod proto;
mod protocol;
mod rewards;
mod session;
mod stash;
mod worldgen;

use std::io;
use std::net::TcpListener;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use std::time::Duration;

use aether_api::{FjallStore, World};

use config::Config;
use players::{Registry, SharedRegistry};
use session::DemoWorld;

/// Flush dirty sub-chunks to disk every `secs` seconds.
///
/// The accept loop never returns and this build has no signal handler, so a
/// Ctrl-C loses whatever was edited since the last tick. Keeping the interval
/// short is the whole durability story for now — see the note in the report.
fn spawn_autosave(world: Arc<DemoWorld>, secs: u64) {
    let period = Duration::from_secs(secs.max(1));
    std::thread::spawn(move || loop {
        std::thread::sleep(period);
        if let Err(e) = world.flush() {
            eprintln!("autosave failed: {e}");
        }
    });
}

fn main() -> std::process::ExitCode {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "aether-server.toml".to_string());
    let (cfg, created) = match Config::load_or_init(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("config error: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if created {
        println!("No config at `{path}` — wrote a sample there; using defaults.\n");
    }

    let store = match FjallStore::open(&cfg.server.world_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: cannot open world at `{}`: {e}",
                cfg.server.world_dir
            );
            return std::process::ExitCode::FAILURE;
        }
    };
    let generator = worldgen::Generator::load(
        &cfg.server.worldgen_data,
        &cfg.server.biome_data,
        cfg.server.seed,
    );
    let described = generator.describe(cfg.server.seed);
    // The generator is deterministic, so its output is a cache and never a
    // source of truth: see `gencache`. Edits still live in the journal.
    let generator = gencache::Cached::new(generator, cfg.server.seed, &cfg.cache.to_config());
    let mut world = World::new(store, generator);
    if let Some(sink) = open_history_mirror(&cfg.database) {
        world = world.with_journal_sink(sink);
    }
    let world: Arc<DemoWorld> = Arc::new(world);
    economy::install(open_economy(&cfg.database));
    let _ = OPERATORS.set(cfg.operators.clone());
    let _ = GAME_MODE.set(cfg.server.game_mode);
    let cfg = Arc::new(cfg);
    let next_eid = Arc::new(AtomicI32::new(1));
    let registry: SharedRegistry = Arc::new(Registry::default());
    // The world ticks on its own thread, independently of whether anyone is
    // connected: time, mobs, items on the floor, furnaces.
    game::tick::start(Arc::clone(&registry), Arc::clone(&world));

    let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind {addr}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let versions: Vec<String> = protocol::codecs()
        .iter()
        .map(|c| format!("{} ({})", c.version_name(), c.protocol_id()))
        .collect();

    println!("╔══════════════════════════════════════════════╗");
    println!("║           Aether Engine — server             ║");
    println!("╚══════════════════════════════════════════════╝");
    println!("listening  : {addr}");
    println!("protocols  : {}", versions.join(", "));
    println!("world      : {described}, {:?}", cfg.server.game_mode);
    println!(
        "saved to   : {} (autosave every {}s)",
        cfg.server.world_dir, cfg.server.autosave_secs
    );
    println!("Ctrl-C to stop.\n");

    spawn_autosave(Arc::clone(&world), cfg.server.autosave_secs);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let world = Arc::clone(&world);
                let cfg = Arc::clone(&cfg);
                let next_eid = Arc::clone(&next_eid);
                let registry = Arc::clone(&registry);
                std::thread::spawn(move || {
                    let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                    if let Err(e) = session::serve(s, &world, &cfg, &next_eid, &registry) {
                        if e.kind() != io::ErrorKind::UnexpectedEof {
                            eprintln!("[{peer}] disconnected: {e}");
                        }
                    }
                });
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Start the history mirror, if one is configured and compiled in.
///
/// Every failure path here returns `None` and prints why. A server whose audit
/// database is unreachable must still start: the journal on disk is the source
/// of truth and it is unaffected.
fn open_history_mirror(cfg: &config::DatabaseConfig) -> Option<aether_world::journal::BatchingSink> {
    let url = cfg.connection_url();
    if url.is_empty() {
        return None;
    }
    let batch = aether_world::journal::BatchConfig {
        max_batch: cfg.batch_size.max(1),
        max_delay: std::time::Duration::from_millis(cfg.batch_delay_ms.max(1)),
        queue_depth: cfg.queue_depth.max(1),
        ..Default::default()
    };
    #[cfg(feature = "postgres")]
    {
        match db::PostgresSink::connect(&url) {
            Ok(sink) => {
                println!("history    : mirroring to PostgreSQL");
                return Some(aether_world::journal::BatchingSink::start(sink, batch));
            }
            Err(e) => {
                eprintln!("warning: history mirror disabled — cannot reach the database: {e}");
                return None;
            }
        }
    }
    #[cfg(not(feature = "postgres"))]
    {
        let _ = batch;
        eprintln!(
            "warning: [database] url is set but this build has no database support \
             (rebuild with `--features postgres`)"
        );
        None
    }
}

/// Connect the economy, if one is configured and compiled in.
///
/// Unlike the history mirror, a failure here leaves the economy *absent*
/// rather than degraded, and the commands say so. Money has no fallback: see
/// [`economy`] for why.
fn open_economy(cfg: &config::DatabaseConfig) -> Option<Box<dyn economy::Economy>> {
    let url = cfg.connection_url();
    if url.is_empty() {
        return None;
    }
    #[cfg(feature = "postgres")]
    {
        match economy::pg::PgEconomy::connect(&url) {
            Ok(e) => {
                println!("economy    : PostgreSQL");
                return Some(Box::new(e));
            }
            Err(e) => {
                eprintln!("warning: economy disabled — {e}");
                return None;
            }
        }
    }
    #[cfg(not(feature = "postgres"))]
    None
}

/// The operator list, installed at startup.
static OPERATORS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Whether `name` may run administrative commands.
///
/// Case-insensitive, and false for everyone when the list is empty — the safe
/// default for an offline-mode server, where a username is a claim rather than
/// an identity.
pub fn is_operator(name: &str) -> bool {
    OPERATORS
        .get()
        .is_some_and(|ops| ops.iter().any(|o| o.eq_ignore_ascii_case(name)))
}

/// The mode this server runs, installed at startup.
static GAME_MODE: std::sync::OnceLock<protocol::GameMode> = std::sync::OnceLock::new();

/// The server's game mode. Creative until configured otherwise, which is what
/// every version of this server did before the setting existed.
pub fn server_game_mode() -> protocol::GameMode {
    GAME_MODE.get().copied().unwrap_or_default()
}
