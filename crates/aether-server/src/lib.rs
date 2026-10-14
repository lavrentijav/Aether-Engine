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

pub mod commands;
pub mod config;
pub mod connections;
pub mod db;
pub mod economy;
pub mod game;
pub mod gencache;
pub mod ground;
pub mod inventory;
pub mod log;
pub mod ops;
pub mod outbox;
pub mod placement;
pub mod players;
pub mod proto;
pub mod protocol;
pub mod rewards;
pub mod session;
pub mod stash;
pub mod stored;
pub mod visibility;
pub mod worldgen;

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
        // The scheduled queues are not part of the world's dirty set, so they
        // are written here, beside the chunks they belong to.
        game::blockticks::save(&world);
        game::fluids::save(&world);
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

/// Run the server: argv parsing, config, world setup, accept loop. Called by
/// the thin `aether-server` binary.
pub fn run() -> std::process::ExitCode {
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
    let mut world = World::new(store, generator)
        .with_snapshot_policy(snapshot_policy(revision))
        // Reload a chunk's pending scheduled ticks as it comes in; they were
        // written next to it (see `game::tickstore`).
        .with_column_load_hook(|world, cx, cz| {
            game::blockticks::load_column(world, cx, cz);
            game::fluids::load_column(world, cx, cz);
        });
    if let Some(sink) = open_history_mirror(&cfg.database) {
        world = world.with_journal_sink(sink);
    }
    let world: Arc<DemoWorld> = Arc::new(world);
    // Item id ⇄ name for the surfaces that still speak names — chat, /audit,
    // the PostgreSQL mirror, the economy rows. The journal itself carries only
    // ids; a modded item's name lives in this world's registry, so hand it to
    // the library once, the same shape as the uid allocator just below.
    {
        let name_world = Arc::clone(&world);
        let id_world = Arc::clone(&world);
        aether_world::registry::items::install_resolvers(
            Box::new(move |id| name_world.item_name(id)),
            Box::new(move |name| id_world.item_id(name)),
        );
    }
    // Item instance ids come from one process-wide allocator over the same KV
    // backend as the journal, installed here — before the listener exists, so
    // no connection can ever mint an id. Opening it reads the reservation
    // bound, so a restart resumes above every id the previous run handed out:
    // an id is never reused, which is the guarantee the ledger's dupes rest on.
    match aether_world::journal::UidAllocator::open(Arc::clone(world.journal().backend())) {
        Ok(alloc) => {
            log::info(&format!("item uids  : next id {}", alloc.head()));
            aether_world::journal::install_allocator(alloc);
        }
        Err(e) => return fail(&format!("cannot open the item uid allocator: {e}")),
    }
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
    warn_if_exposed(&cfg);

    spawn_autosave(Arc::clone(&world), cfg.server.autosave_secs);
    ops::install_signal_handlers();
    ops::spawn_shutdown(Arc::clone(&registry), Arc::clone(&world));
    ops::spawn_monitor(
        Arc::clone(&registry),
        cfg.server.watchdog_secs,
        cfg.server.status_secs,
    );
    log::info("ready (SIGINT or SIGTERM stops cleanly)");

    // One thread per connection is the whole cost model, so cap how many we
    // will run at once. A refusal drops the socket here, before any thread
    // exists, and is logged sparingly so a flood cannot bury the log.
    let limiter = connections::Limiter::new(
        cfg.server.connection_limit() as usize,
        cfg.server.per_ip_connection_limit() as usize,
    );

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let ip = s.peer_addr().ok().map(|a| a.ip());
                let Some(guard) = limiter.try_acquire(ip) else {
                    limiter.warn_refused(ip);
                    continue;
                };
                let world = Arc::clone(&world);
                let cfg = Arc::clone(&cfg);
                let next_eid = Arc::clone(&next_eid);
                let registry = Arc::clone(&registry);
                std::thread::spawn(move || {
                    // Held for the connection's whole life; the thread ending
                    // — normally or by panic — drops it and frees the slot.
                    let _guard = guard;
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
    let sink = match db::PostgresSink::connect(&url) {
        Ok(sink) => {
            log::info("history    : mirroring to PostgreSQL");
            Some(aether_world::journal::BatchingSink::start(sink, batch))
        }
        Err(e) => {
            log::warn(&format!(
                "history mirror disabled: cannot reach the database: {e}"
            ));
            None
        }
    };
    // Refused at startup; see `main`.
    #[cfg(not(feature = "postgres"))]
    let sink = {
        let _ = batch;
        None
    };
    sink
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
    let economy: Option<Box<dyn economy::Economy>> = match economy::pg::PgEconomy::connect(&url) {
        Ok(e) => {
            log::info("economy    : PostgreSQL");
            Some(Box::new(e))
        }
        Err(e) => {
            log::warn(&format!("economy disabled: {e}"));
            None
        }
    };
    #[cfg(not(feature = "postgres"))]
    let economy = None;
    economy
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

/// The `[visibility]` settings, or the defaults (off) before startup installs
/// the config.
pub fn visibility_config() -> config::VisibilityConfig {
    CONFIG
        .get()
        .map(|c| c.visibility.clone())
        .unwrap_or_default()
}

/// Whether visibility culling is on. A single load and branch, for the block
/// write path, which must cost nothing on the default (off) configuration.
#[inline]
pub fn visibility_enabled() -> bool {
    CONFIG.get().is_some_and(|c| c.visibility.enabled)
}

/// The chunk radius kept loaded around a player, from the config.
///
/// Clamped the same way the streamer clamps it, so the visibility search and
/// the columns actually sent cannot disagree about how far the view reaches.
pub fn server_view_radius() -> i32 {
    CONFIG
        .get()
        .map(|c| c.server.clamped_view_radius())
        .unwrap_or(8)
}

/// The mode this server runs, installed at startup.
static GAME_MODE: std::sync::OnceLock<protocol::GameMode> = std::sync::OnceLock::new();

/// The server's game mode. Creative until configured otherwise, which is what
/// every version of this server did before the setting existed.
pub fn server_game_mode() -> protocol::GameMode {
    GAME_MODE.get().copied().unwrap_or_default()
}
