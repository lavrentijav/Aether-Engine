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
mod config;
mod db;
mod economy;
mod game;
mod gencache;
mod ground;
mod inventory;
mod log;
mod ops;
mod outbox;
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

/// Flush dirty sub-chunks to disk every `secs` seconds. A clean stop
/// (SIGINT/SIGTERM, see [`ops`]) flushes too; this bounds what a crash loses.
fn spawn_autosave(world: Arc<DemoWorld>, secs: u64) {
    let period = Duration::from_secs(secs.max(1));
    std::thread::spawn(move || loop {
        std::thread::sleep(period);
        if let Err(e) = world.flush() {
            log::error(&format!("autosave failed: {e}"));
        }
    });
}

/// Refuse to start, saying why.
fn fail(msg: &str) -> std::process::ExitCode {
    log::error(msg);
    std::process::ExitCode::FAILURE
}

fn main() -> std::process::ExitCode {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "aether-server.toml".to_string());
    let (mut cfg, created) = match Config::load_or_init(&path) {
        Ok(v) => v,
        Err(e) => return fail(&format!("config error: {e}")),
    };
    if created {
        log::warn(&format!(
            "no config at `{path}`: wrote a sample there (see aether-server.example.toml); using defaults"
        ));
    }
    if let Err(e) = cfg.apply_env(|k| std::env::var(k).ok()) {
        return fail(&format!("config error: {e}"));
    }
    #[cfg(not(feature = "postgres"))]
    if !cfg.database.connection_url().is_empty() {
        return fail(
            "a database is configured ([database] url or AETHER_DATABASE_URL) but this build \
             has no database support: rebuild with `--features postgres`, or unset it",
        );
    }

    let store = match FjallStore::open(&cfg.server.world_dir) {
        Ok(s) => s,
        Err(e) => {
            return fail(&format!(
                "cannot open world at `{}`: {e}",
                cfg.server.world_dir
            ))
        }
    };
    let generator = match worldgen::Generator::load(
        &cfg.server.worldgen_data,
        &cfg.server.biome_data,
        cfg.server.seed,
    ) {
        Ok(g) => g,
        Err(e) => {
            return fail(&format!(
                "worldgen_data `{}` is set but unusable: {e}. Fetch the game's data pack with \
                 tools/fetch-vanilla-data.sh, or set worldgen_data = \"\" for noise terrain",
                cfg.server.worldgen_data
            ))
        }
    };
    let described = generator.describe(cfg.server.seed);
    let revision = generator.revision();
    // The generator is deterministic, so its output is a cache and never a
    // source of truth: see `gencache`. Edits still live in the journal.
    let generator = gencache::Cached::with_revision(
        generator,
        cfg.server.seed,
        revision,
        &cfg.cache.to_config(),
    );
    let mut world = World::new(store, generator).with_snapshot_policy(snapshot_policy(revision));
    if let Some(sink) = open_history_mirror(&cfg.database) {
        world = world.with_journal_sink(sink);
    }
    let world: Arc<DemoWorld> = Arc::new(world);
    economy::install(open_economy(&cfg.database));
    let _ = GAME_MODE.set(cfg.server.game_mode);
    let cfg = Arc::new(cfg);
    let _ = CONFIG.set(Arc::clone(&cfg));
    let next_eid = Arc::new(AtomicI32::new(1));
    let registry: SharedRegistry = Arc::new(Registry::default());
    // The world ticks on its own thread, independently of whether anyone is
    // connected: time, mobs, items on the floor, furnaces.
    game::tick::start(Arc::clone(&registry), Arc::clone(&world));

    let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => return fail(&format!("cannot bind {addr}: {e}")),
    };

    let versions: Vec<String> = protocol::codecs()
        .iter()
        .map(|c| format!("{} ({})", c.version_name(), c.protocol_id()))
        .collect();

    log::info(&format!(
        "Aether Engine server {}",
        env!("CARGO_PKG_VERSION")
    ));
    log::info(&format!("listening  : {addr}"));
    log::info(&format!("protocols  : {}", versions.join(", ")));
    log::info(&format!(
        "world      : {described}, {:?}",
        cfg.server.game_mode
    ));
    log::info(&format!(
        "saved to   : {} (autosave every {}s)",
        cfg.server.world_dir, cfg.server.autosave_secs
    ));
    log::info(&ops::radius_estimate(&cfg.server));
    warn_if_exposed(&cfg);

    spawn_autosave(Arc::clone(&world), cfg.server.autosave_secs);
    ops::install_signal_handlers();
    ops::spawn_shutdown(Arc::clone(&registry), Arc::clone(&world));
    ops::spawn_monitor(
        Arc::clone(&registry),
        Arc::clone(&world),
        cfg.server.watchdog_secs,
        cfg.server.status_secs,
    );
    ops::spawn_residency(
        Arc::clone(&registry),
        Arc::clone(&world),
        cfg.server.max_resident_columns,
    );
    log::info("ready (SIGINT or SIGTERM stops cleanly)");

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
                            log::info(&format!("[{peer}] connection ended: {e}"));
                        }
                    }
                });
            }
            Err(e) => log::warn(&format!("accept failed: {e}")),
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Sub-chunk snapshots carry the generator revision they were saved under;
/// a stale one is rebuilt over the fresh terrain as it loads (see
/// [`World::with_snapshot_policy`]). Without this, sub-chunks edited under an
/// older generator kept its terrain whole — bare stone between neighbours the
/// new one had given grass and trees.
fn snapshot_policy(revision: u32) -> aether_api::SnapshotPolicy {
    aether_api::SnapshotPolicy {
        revision,
        keep: Box::new(|id| !is_generated_terrain(id)),
        on_rebuild: Box::new(|key, kept| {
            log::info(&format!(
                "rebuilt sub-chunk ({}, {}, {}) saved by an older generator; kept {kept} placed block(s)",
                key.cx, key.cy, key.cz
            ))
        }),
    }
}

/// Whether a block is terrain a generator lays down — rock, soil, fluid, air
/// — rather than something a player is likely to have placed. In a stale
/// snapshot these are the old generator's output and give way to the new
/// one's; everything else is kept. Edits the journal recorded are replayed
/// afterwards either way, so a placed block of stone or dirt still returns.
fn is_generated_terrain(id: aether_api::BlockStateId) -> bool {
    let Some((_, name)) = aether_world::registry::blocks::block_of_state(id) else {
        return false;
    };
    matches!(
        name.trim_start_matches("minecraft:"),
        "air"
            | "cave_air"
            | "void_air"
            | "bedrock"
            | "stone"
            | "deepslate"
            | "tuff"
            | "granite"
            | "diorite"
            | "andesite"
            | "calcite"
            | "dirt"
            | "coarse_dirt"
            | "rooted_dirt"
            | "grass_block"
            | "podzol"
            | "mycelium"
            | "mud"
            | "clay"
            | "gravel"
            | "sand"
            | "red_sand"
            | "sandstone"
            | "red_sandstone"
            | "terracotta"
            | "snow"
            | "snow_block"
            | "powder_snow"
            | "ice"
            | "packed_ice"
            | "water"
            | "lava"
            | "netherrack"
            | "end_stone"
    )
}

/// Start the history mirror, if one is configured and compiled in.
///
/// Every failure path here returns `None` and prints why. A server whose audit
/// database is unreachable must still start: the journal on disk is the source
/// of truth and it is unaffected.
fn open_history_mirror(
    cfg: &config::DatabaseConfig,
) -> Option<aether_world::journal::BatchingSink> {
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
                log::info("history    : mirroring to PostgreSQL");
                return Some(aether_world::journal::BatchingSink::start(sink, batch));
            }
            Err(e) => {
                log::warn(&format!(
                    "history mirror disabled: cannot reach the database: {e}"
                ));
                return None;
            }
        }
    }
    #[cfg(not(feature = "postgres"))]
    {
        // Refused at startup; see `main`.
        let _ = batch;
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
                log::info("economy    : PostgreSQL");
                return Some(Box::new(e));
            }
            Err(e) => {
                log::warn(&format!("economy disabled: {e}"));
                return None;
            }
        }
    }
    #[cfg(not(feature = "postgres"))]
    None
}

/// The configuration, installed at startup.
static CONFIG: std::sync::OnceLock<Arc<Config>> = std::sync::OnceLock::new();

/// Whether this player may run administrative commands: their name is an
/// operator entry, and if the entry names an address, they connected from it.
/// False for everyone when the list is empty — the safe default for an
/// offline-mode server, where a username is a claim rather than an identity.
pub fn is_operator(handle: &players::PlayerHandle) -> bool {
    CONFIG
        .get()
        .is_some_and(|c| c.is_operator(&handle.name, handle.ip))
}

/// Say so, loudly, when a server anyone can reach trusts names alone.
fn warn_if_exposed(cfg: &Config) {
    if !cfg.is_public() {
        return;
    }
    let unbound = cfg.unbound_operators();
    if cfg.whitelist.is_empty() && !unbound.is_empty() {
        log::warn(&format!(
            "listening on {} in offline mode with operators bound to no address ({}): \
             anyone who joins under one of those names is an operator. Write them as \
             \"name@address\", or set a whitelist",
            cfg.server.host,
            unbound.join(", ")
        ));
    } else if cfg.whitelist.is_empty() {
        log::warn(&format!(
            "listening on {} in offline mode with no whitelist: anyone can join under any name",
            cfg.server.host
        ));
    }
}

/// The chunk radius to stream to a client whose own view distance is
/// `client`: see [`config::ServerConfig::radius_for`].
pub fn view_radius_for(client: Option<u8>) -> i32 {
    CONFIG
        .get()
        .map(|c| c.server.radius_for(client))
        .unwrap_or(8)
}

/// The mode this server runs, installed at startup.
static GAME_MODE: std::sync::OnceLock<protocol::GameMode> = std::sync::OnceLock::new();

/// The server's game mode. Creative until configured otherwise, which is what
/// every version of this server did before the setting existed.
pub fn server_game_mode() -> protocol::GameMode {
    GAME_MODE.get().copied().unwrap_or_default()
}
