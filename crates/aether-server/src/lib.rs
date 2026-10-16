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

pub mod auth;
pub mod chatlog;
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
pub mod permissions;
pub mod placement;
pub mod players;
pub mod plugins;
pub mod proto;
pub mod protocol;
pub mod rewards;
pub mod session;
pub mod staff;
pub mod stash;
pub mod stored;
pub mod visibility;
pub mod web;
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
    #[cfg(not(feature = "plugins"))]
    if !cfg.plugins.dir.trim().is_empty() {
        log::warn(
            "a plugin directory is configured but this build has no plugin support: \
             rebuild with the `plugins` feature (the default), or unset it",
        );
    }

    // Online-mode key and HTTP client, before the world opens: a hybrid
    // server that cannot reach Mojang must say so at startup, not refuse
    // every premium login later. Offline is a no-op here.
    if let Err(e) = auth::install(&cfg.auth) {
        return fail(&format!("auth error: {e}"));
    }
    log::info(&format!("auth       : {:?}", cfg.auth.mode));

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
    // The recipe book, read from the same pack the terrain is: crafting,
    // smelting, smithing and the rest therefore match the version the world
    // speaks. A packless server keeps the compiled-in tables. A pack set but
    // unreadable is a warning, not a refusal: recipes are gameplay, and the
    // world is already being generated.
    if !cfg.server.worldgen_data.is_empty() {
        match game::recipes::load(std::path::Path::new(&cfg.server.worldgen_data)) {
            Ok(book) => {
                let c = book.counts();
                log::info(&format!(
                    "recipes    : {} loaded from the pack ({} shaped, {} shapeless, {} cooking, \
                     {} stonecutting, {} smithing, {} transmute; {} special, {} unsupported)",
                    c.total,
                    c.shaped,
                    c.shapeless,
                    c.cooking,
                    c.stonecutting,
                    c.smithing,
                    c.transmute,
                    c.special,
                    c.unsupported.len(),
                ));
                if c.special > 0 {
                    log::info(&format!(
                        "recipes    : {} dynamic recipes not run (stubbed): {}",
                        c.special,
                        c.special_list.join(", ")
                    ));
                }
                game::recipes::install(book);
            }
            Err(e) => log::warn(&format!(
                "recipes    : cannot load from `{}`: {e}; using the compiled-in table",
                cfg.server.worldgen_data
            )),
        }
    }
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
            // Generated fluid that can flow needs a first tick, since the
            // generator schedules none.
            game::fluids::wake_generated(world, cx, cz);
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
    // The permission store is the source of truth for every level and node
    // check; seed it from `config.operators` before anyone can connect.
    permissions::install(&cfg);
    let _ = GAME_MODE.set(cfg.server.game_mode);
    let cfg = Arc::new(cfg);
    let _ = CONFIG.set(Arc::clone(&cfg));
    // Load the WebAssembly plugins after the world exists, so their
    // `storage.kv` has somewhere to live, and before the listener accepts a
    // connection, so no event can fire before `init` has run. The player
    // registry is created first so the SDK's message/player services can reach
    // it through the host backend.
    let registry: SharedRegistry = Arc::new(Registry::default());
    plugins::install(&cfg, &world, &registry);
    let next_eid = Arc::new(AtomicI32::new(1));
    // The world ticks on its own thread, independently of whether anyone is
    // connected: time, mobs, items on the floor, furnaces.
    game::tick::start(Arc::clone(&registry), Arc::clone(&world));
    // Region activity is sampled for the web admin's heat map on its own
    // thread, once a second, so it never touches the tick thread's budget.
    web::spawn_sampler();

    // Loot tables are read from the same pack. Parse them once now so the
    // report of what the parser skipped is complete before it is logged.
    let loot = game::loot::LootTables::global();
    loot.warm();
    let mut skipped = loot.unsupported();
    if !skipped.is_empty() {
        skipped.sort();
        log::info(&format!(
            "loot tables : {} construct(s) parsed as no-op or skipped: {}{}",
            skipped.len(),
            skipped
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if skipped.len() > 8 { ", …" } else { "" }
        ));
    }

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
    log::info(&format!("anti-xray  : {}", anti_xray_summary(&cfg)));
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

/// Whether this player may run administrative commands: their effective
/// permission level is at least 1.
///
/// The level comes from the permission store ([`permissions`]), seeded at
/// startup from `config.operators` — a member of the built-in `op` group is
/// level 4. False for everyone the store does not know, the safe default for an
/// offline-mode server, where a username is a claim rather than an identity.
pub fn is_operator(handle: &players::PlayerHandle) -> bool {
    permissions::is_operator(&handle.name, handle.ip)
}

/// The operator's `worldgen_data` path, for the parts of the game that read
/// the same data pack the generator does (loot tables, recipes). Empty when
/// no pack is configured.
pub fn worldgen_data() -> &'static str {
    CONFIG
        .get()
        .map(|c| c.server.worldgen_data.as_str())
        .unwrap_or("")
}

/// The world seed, for the subsystems that derive a deterministic random from
/// it (structure chest loot).
pub fn seed() -> u64 {
    CONFIG.get().map(|c| c.server.seed).unwrap_or(0)
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

/// The permission node that lets a player bypass anti-xray.
///
/// **Not implied by being an operator.** Until the permission store (CMD-01)
/// lands, it is granted explicitly through `[permissions] grants` (see
/// [`config::PermissionsConfig`]); the node is named here so the store can
/// resolve it without a rename later.
pub const ANTIXRAY_BYPASS_PERMISSION: &str = "aether.antixray.bypass";

/// The `[anti_xray]` settings, or the defaults (off) before startup installs
/// the config.
pub fn anti_xray_config() -> config::AntiXrayConfig {
    CONFIG
        .get()
        .map(|c| c.anti_xray.clone())
        .unwrap_or_default()
}

/// The `[chat]` settings, or the defaults before startup installs the config.
/// The defaults log everything with the built-in redact list, so chat logging
/// works even in a test that never installed a config.
pub fn chat_config() -> config::ChatConfig {
    CONFIG.get().map(|c| c.chat.clone()).unwrap_or_default()
}

/// Whether anti-xray is active for the running world, resolved once.
///
/// The block-write path calls this for every write, so the per-world test
/// (`worlds`) is computed once after the config is installed rather than
/// re-parsing the world directory each time. `0` means "not asked yet": a call
/// before the config exists returns `false` without caching, so the answer
/// becomes correct once startup installs it.
static ANTI_XRAY_ACTIVE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Whether anti-xray is on for the running world. A single load and branch,
/// for the block write path, which must cost nothing on the default (off)
/// configuration.
#[inline]
pub fn anti_xray_enabled() -> bool {
    const ON: u8 = 1;
    const OFF: u8 = 2;
    use std::sync::atomic::Ordering;
    match ANTI_XRAY_ACTIVE.load(Ordering::Relaxed) {
        ON => true,
        OFF => false,
        _ => {
            let on = CONFIG.get().is_some_and(|c| {
                c.anti_xray.enabled && c.anti_xray.applies_to_world(&c.server.world_dir)
            });
            if CONFIG.get().is_some() {
                ANTI_XRAY_ACTIVE.store(if on { ON } else { OFF }, Ordering::Relaxed);
            }
            on
        }
    }
}

/// A one-line description of the active anti-xray configuration, for the
/// startup log — so an operator can see at a glance which mode is live and for
/// which worlds, rather than inferring it from a silent config.
///
/// It reports the explicit bypass list, never "operators bypass": operators are
/// not bypassed automatically (see [`anti_xray_bypass`], SP1).
pub fn anti_xray_summary(cfg: &config::Config) -> String {
    use config::AntiXrayMode;
    let a = &cfg.anti_xray;
    let worlds = if a.worlds.is_empty() {
        "all worlds".to_string()
    } else {
        format!("worlds: {}", a.worlds.join(", "))
    };
    let applies = a.applies_to_world(&cfg.server.world_dir);
    if !a.enabled {
        return "off".to_string();
    }
    if !applies {
        return format!(
            "off for this world (`{}` not in {})",
            cfg.server.world_dir, worlds
        );
    }
    let bypass = if a.bypass_players.is_empty() {
        "no automatic bypass".to_string()
    } else {
        format!("bypass: {}", a.bypass_players.join(", "))
    };
    match a.mode {
        AntiXrayMode::Engine => format!(
            "engine — {} hidden block(s), {worlds}, {bypass}",
            a.hidden_block_names().len()
        ),
        AntiXrayMode::Culling => format!(
            "culling — margin {} blocks, fill {:?}, reveal budget {}/tick, {worlds}, {bypass}",
            a.margin_blocks, a.hidden_fill, a.reveal_budget_per_tick
        ),
    }
}

/// Whether `handle` may see everything.
///
/// **Being an operator is not enough.** Bypass is always explicit: the
/// `/spectate xray` override, the per-player `/visibility off` toggle, the
/// [`ANTIXRAY_BYPASS_PERMISSION`] node granted through `[permissions]`, or a
/// name in `[anti_xray] bypass_players`. An operator who wants to watch the
/// protection work is therefore left under it by default — and can turn it off
/// with `/visibility off` when they need to.
pub fn anti_xray_bypass(handle: &players::PlayerHandle) -> bool {
    let (node, listed) = match CONFIG.get() {
        Some(c) => (
            c.permissions
                .granted(&handle.name, handle.ip, ANTIXRAY_BYPASS_PERMISSION),
            c.anti_xray
                .bypass_players
                .iter()
                .any(|n| n.eq_ignore_ascii_case(&handle.name)),
        ),
        None => (false, false),
    };
    bypass_decision(
        handle.staff().xray(),
        handle.visibility().bypass,
        node || listed,
    )
}

/// The bypass decision itself, without a handle, so it can be tested and
/// reused: the `/spectate xray` override wins over everything, then the
/// explicit per-player toggle or grant.
///
/// `Off` shows everything; `engine`/`culling`/`<margin>` *force anti-xray on*
/// even for someone who would otherwise bypass it; `Default` falls back to the
/// explicit toggle/grant.
pub fn bypass_decision(level: staff::AntiXrayLevel, toggle: bool, explicit_grant: bool) -> bool {
    match level {
        staff::AntiXrayLevel::Off => true,
        staff::AntiXrayLevel::Engine
        | staff::AntiXrayLevel::Culling
        | staff::AntiXrayLevel::Margin(_) => false,
        staff::AntiXrayLevel::Default => toggle || explicit_grant,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_anti_xray_summary_reports_the_live_mode() {
        let mut cfg = config::Config::default();
        assert_eq!(anti_xray_summary(&cfg), "off");

        cfg.anti_xray.enabled = true;
        cfg.anti_xray.mode = config::AntiXrayMode::Culling;
        let s = anti_xray_summary(&cfg);
        assert!(s.starts_with("culling"), "{s}");
        assert!(s.contains("margin 5"), "{s}");
        assert!(s.contains("all worlds"), "{s}");

        // A world the config does not list is reported off for that world.
        cfg.anti_xray.mode = config::AntiXrayMode::Engine;
        cfg.anti_xray.worlds = vec!["alpha".to_string()];
        cfg.server.world_dir = "beta".to_string();
        let s = anti_xray_summary(&cfg);
        assert!(s.contains("off for this world"), "{s}");

        // No "operators bypass": SP1 made bypass explicit only.
        let mut cfg = config::Config::default();
        cfg.anti_xray.enabled = true;
        let s = anti_xray_summary(&cfg);
        assert!(s.contains("no automatic bypass"), "{s}");
        assert!(!s.contains("operators bypass"), "{s}");
    }

    /// An operator with neither the toggle nor a grant is **not** bypassed, and
    /// each explicit path works; a forced spectate level beats a grant.
    #[test]
    fn bypass_is_only_explicit() {
        use super::staff::AntiXrayLevel;
        // The regression this covers: previously `is_operator` bypassed an
        // operator, so they could not watch the anti-xray protection work.
        assert!(!bypass_decision(AntiXrayLevel::Default, false, false));
        // `/visibility off` (the per-player toggle).
        assert!(bypass_decision(AntiXrayLevel::Default, true, false));
        // The `aether.antixray.bypass` node or `[anti_xray] bypass_players`.
        assert!(bypass_decision(AntiXrayLevel::Default, false, true));
        // `/spectate xray off`.
        assert!(bypass_decision(AntiXrayLevel::Off, false, false));
        // The override wins: an explicitly-granted admin still sees masking at
        // a forced level, which is the point of per-admin control.
        for level in [
            AntiXrayLevel::Engine,
            AntiXrayLevel::Culling,
            AntiXrayLevel::Margin(8),
        ] {
            assert!(!bypass_decision(level, true, true), "{level:?}");
        }
    }
}
