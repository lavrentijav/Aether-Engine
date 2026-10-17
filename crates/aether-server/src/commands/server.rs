//! Vanilla server control and information commands (CMD-09).
//!
//! ```text
//! /stop
//! /save-all [flush]
//! /save-on
//! /save-off
//! /list [uuids]
//! /seed
//! /version
//! /setidletimeout <minutes>
//! /transfer <hostname> [<port>] [<players>]
//! ```
//!
//! The wording and grammar are 1.21.11 `en_us` and `commands.json`. Two things
//! differ from the brief but match the live dedicated server: `/seed` takes
//! **no** argument in 1.21.11, and `/publish` is **absent** from the dedicated
//! command tree (it belongs to the singleplayer integrated server), so it is
//! not declared here at all.
//!
//! `/save-all` and `/stop` use the same save and stop paths as the signal
//! handler. A full save takes the shared save lock, so `/save-all`, the
//! autosave thread and shutdown cannot run two flushes at once.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::admin::{resolve_targets, Target};
use crate::commands::feedback::lang;
use crate::commands::tree::{self, Builder, Ctx, Node, Requirement, Value};
use crate::commands::Reply;
use crate::players::PlayerHandle;
use crate::session::DemoWorld;

/// Vanilla 1.21.11 `en_us` strings this module emits, verbatim.
mod text {
    pub const SAVING: &str = "Saving the game (this may take a moment!)";
    pub const SAVED: &str = "Saved the game";
    pub const SAVE_FAILED: &str = "Unable to save the game (is there enough disk space?)";
    pub const SAVE_ENABLED: &str = "Automatic saving is now enabled";
    pub const SAVE_DISABLED: &str = "Automatic saving is now disabled";
    pub const SAVE_ALREADY_ON: &str = "Saving is already turned on";
    pub const SAVE_ALREADY_OFF: &str = "Saving is already turned off";
    pub const LIST: &str = "There are %s of a max of %s players online: %s";
    pub const NAME_AND_ID: &str = "%s (%s)";
    pub const SEED: &str = "Seed: [%s]";
    pub const VERSION_HEADER: &str = "Server version info:";
    pub const VERSION_ID: &str = "id = %s";
    pub const VERSION_NAME: &str = "name = %s";
    pub const VERSION_DATA: &str = "data = %s";
    pub const VERSION_SERIES: &str = "series = %s";
    pub const VERSION_PROTOCOL: &str = "protocol = %s (%s)";
    pub const VERSION_BUILD_TIME: &str = "build_time = %s";
    pub const VERSION_PACK_RESOURCE: &str = "pack_resource = %s";
    pub const VERSION_PACK_DATA: &str = "pack_data = %s";
    pub const VERSION_STABLE: &str = "stable = %s";
    pub const IDLE_SET: &str = "The player idle timeout is now %s minute(s)";
    pub const IDLE_DISABLED: &str = "The player idle timeout is now disabled";
    pub const TRANSFER_SINGLE: &str = "Transferring %s to %s:%s";
    pub const TRANSFER_MULTIPLE: &str = "Transferring %s players to %s:%s";
    pub const PLAYER_REQUIRED: &str = "A player is required to run this command here";
    pub const NO_PLAYER: &str = "No player was found";

    /// The values `/version` reports for Aether. `data`, `pack_resource` and
    /// `pack_data` are the 1.21.11 data/resource pack generation the engine's
    /// tables are built from; `protocol` spans every modern codec; `stable` is
    /// `no` because the server is an early preview, and `build_time` comes from
    /// `AETHER_BUILD_TIME` when a build sets it.
    pub const AETHER_NAME: &str = "Aether Engine";
    pub const AETHER_SERIES: &str = "main";
    pub const AETHER_DATA: &str = "4671";
    pub const AETHER_PROTOCOL: &str = "774-777";
    pub const AETHER_PROTOCOL_HEX: &str = "0x306-0x309";
    pub const AETHER_PACK_RESOURCE: &str = "75.0";
    pub const AETHER_PACK_DATA: &str = "94.1";
    pub const AETHER_STABLE: &str = "no";
    pub const AETHER_BUILD_TIME: &str = match option_env!("AETHER_BUILD_TIME") {
        Some(time) => time,
        None => "unknown",
    };
}

/// Automatic saves (the periodic chunk/clock flush and player snapshots).
static AUTOSAVE: AtomicBool = AtomicBool::new(true);
/// Minutes a player may be silent before being kicked; `0` disables it.
static IDLE_TIMEOUT_MINUTES: AtomicU64 = AtomicU64::new(0);
/// Serialises explicit and automatic full saves so a `/save-all` arriving
/// during an autosave waits rather than running two flushes at once.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// Whether automatic saving is on (the `/save-on` state).
pub(crate) fn autosave_enabled() -> bool {
    AUTOSAVE.load(Ordering::Relaxed)
}

/// The idle timeout, or `None` when it is disabled.
pub(crate) fn idle_timeout() -> Option<Duration> {
    let minutes = IDLE_TIMEOUT_MINUTES.load(Ordering::Relaxed);
    (minutes > 0).then(|| Duration::from_secs(minutes.saturating_mul(60)))
}

#[cfg(test)]
fn idle_timeout_minutes() -> u64 {
    IDLE_TIMEOUT_MINUTES.load(Ordering::Relaxed)
}

fn set_idle_timeout_minutes(minutes: u64) {
    IDLE_TIMEOUT_MINUTES.store(minutes, Ordering::Relaxed);
}

/// Save every online player's state as well as the world, as `/save-all` (and
/// vanilla's autosave) does.
pub(crate) fn save_all(
    world: &DemoWorld,
    registry: &crate::players::SharedRegistry,
) -> Result<(), String> {
    let players = registry.snapshot();
    save_players_and_world(world, &players)
}

/// The shared save itself: player records, the scheduled queues, the world
/// clock and the storage flush, all under [`SAVE_LOCK`]. `/save-all`, the
/// autosave thread and the shutdown watcher all go through here, so no two of
/// them flush concurrently.
pub(crate) fn save_players_and_world(
    world: &DemoWorld,
    players: &[Arc<PlayerHandle>],
) -> Result<(), String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for player in players {
        if player.full() {
            crate::session::save_inventory(player, world);
            crate::session::save_player(player, world);
        }
    }
    flush_world(world)
}

fn flush_world(world: &DemoWorld) -> Result<(), String> {
    crate::game::blockticks::save(world);
    crate::game::fluids::save(world);
    crate::game::tick::save_time(world);
    world.flush().map_err(|e| e.to_string())
}

fn requirement(op: u8, node: &'static str) -> Requirement {
    Requirement::Level { op, node }
}

/// Every server-control and information command, for the command tree.
pub(crate) fn nodes() -> Vec<Node> {
    vec![
        Builder::literal("stop")
            .requires(requirement(4, "minecraft.command.stop"))
            .executable(exec_stop)
            .build(),
        Builder::literal("save-all")
            .requires(requirement(4, "minecraft.command.save-all"))
            .executable(exec_save_all)
            .then(Builder::literal("flush").executable(exec_save_all))
            .build(),
        Builder::literal("save-on")
            .requires(requirement(4, "minecraft.command.save-on"))
            .executable(exec_save_on)
            .build(),
        Builder::literal("save-off")
            .requires(requirement(4, "minecraft.command.save-off"))
            .executable(exec_save_off)
            .build(),
        Builder::literal("list")
            .executable(exec_list)
            .then(Builder::literal("uuids").executable(exec_list_uuids))
            .build(),
        Builder::literal("seed")
            .requires(requirement(2, "minecraft.command.seed"))
            .executable(exec_seed)
            .build(),
        Builder::literal("version")
            .requires(requirement(2, "minecraft.command.version"))
            .executable(exec_version)
            .build(),
        Builder::literal("setidletimeout")
            .requires(requirement(3, "minecraft.command.setidletimeout"))
            .then(
                Builder::arg(
                    "minutes",
                    tree::ArgKind::IntegerRange {
                        min: Some(0),
                        max: None,
                    },
                )
                .executable(exec_setidletimeout),
            )
            .build(),
        Builder::literal("transfer")
            .requires(requirement(3, "minecraft.command.transfer"))
            .then(
                Builder::arg("hostname", tree::ArgKind::Quotable)
                    .executable(exec_transfer)
                    .then(
                        Builder::arg(
                            "port",
                            tree::ArgKind::IntegerRange {
                                min: Some(1),
                                max: Some(65535),
                            },
                        )
                        .executable(exec_transfer)
                        .then(
                            Builder::arg("players", tree::ArgKind::Players)
                                .executable(exec_transfer),
                        ),
                    ),
            )
            .build(),
    ]
}

fn exec_stop(_ctx: &mut Ctx) -> Reply {
    crate::ops::request_stop();
    Reply::one("Stopping the server")
}

fn exec_save_all(ctx: &mut Ctx) -> Reply {
    // Vanilla reports the start before it writes and the result after. A
    // streaming caller gets the first line immediately; one that does not
    // stream (tests, functions) gets both in the final reply.
    let reported = ctx.report(text::SAVING);
    match save_all(ctx.world, ctx.registry) {
        Ok(()) => {
            if reported {
                Reply::one(text::SAVED)
            } else {
                Reply(vec![text::SAVING.to_string(), text::SAVED.to_string()])
            }
        }
        Err(e) => {
            crate::log::error(&format!("save-all failed: {e}"));
            // The progress line is informational and already went out on the
            // streaming path; only the failure is an error. A non-streaming
            // caller gets the failure alone rather than an error-coloured
            // "Saving…" line.
            ctx.fail(text::SAVE_FAILED)
        }
    }
}

fn exec_save_on(ctx: &mut Ctx) -> Reply {
    if AUTOSAVE.swap(true, Ordering::Relaxed) {
        ctx.fail(text::SAVE_ALREADY_ON)
    } else {
        Reply::one(text::SAVE_ENABLED)
    }
}

fn exec_save_off(ctx: &mut Ctx) -> Reply {
    if AUTOSAVE.swap(false, Ordering::Relaxed) {
        Reply::one(text::SAVE_DISABLED)
    } else {
        ctx.fail(text::SAVE_ALREADY_OFF)
    }
}

fn exec_list(ctx: &mut Ctx) -> Reply {
    list(ctx, false)
}

fn exec_list_uuids(ctx: &mut Ctx) -> Reply {
    list(ctx, true)
}

fn list(ctx: &Ctx, uuids: bool) -> Reply {
    let viewer = ctx.player();
    let mut players: Vec<Arc<PlayerHandle>> = ctx
        .registry
        .snapshot()
        .into_iter()
        .filter(|target| match viewer {
            Some(viewer) => crate::staff::visible_to(viewer, target),
            None => true,
        })
        .collect();
    // Vanilla prints the player list in connection order; entity ids are
    // allocated in that order.
    players.sort_by_key(|player| player.entity_id);
    let names: Vec<String> = players
        .iter()
        .map(|player| {
            if uuids {
                lang::format(
                    text::NAME_AND_ID,
                    &[
                        &player.name,
                        &crate::auth::api::hyphenated_uuid(player.uuid),
                    ],
                )
            } else {
                player.name.clone()
            }
        })
        .collect();
    Reply::one(lang::format(
        text::LIST,
        &[
            &players.len().to_string(),
            &crate::max_players().to_string(),
            &names.join(", "),
        ],
    ))
}

fn exec_seed(_ctx: &mut Ctx) -> Reply {
    Reply::one(lang::format(text::SEED, &[&crate::seed().to_string()]))
}

fn exec_version(_ctx: &mut Ctx) -> Reply {
    Reply(vec![
        text::VERSION_HEADER.to_string(),
        lang::format(text::VERSION_ID, &[env!("CARGO_PKG_VERSION")]),
        lang::format(text::VERSION_NAME, &[text::AETHER_NAME]),
        lang::format(text::VERSION_DATA, &[text::AETHER_DATA]),
        lang::format(text::VERSION_SERIES, &[text::AETHER_SERIES]),
        lang::format(
            text::VERSION_PROTOCOL,
            &[text::AETHER_PROTOCOL, text::AETHER_PROTOCOL_HEX],
        ),
        lang::format(text::VERSION_BUILD_TIME, &[text::AETHER_BUILD_TIME]),
        lang::format(text::VERSION_PACK_RESOURCE, &[text::AETHER_PACK_RESOURCE]),
        lang::format(text::VERSION_PACK_DATA, &[text::AETHER_PACK_DATA]),
        lang::format(text::VERSION_STABLE, &[text::AETHER_STABLE]),
    ])
}

fn exec_setidletimeout(ctx: &mut Ctx) -> Reply {
    let Some(minutes) = int_arg(ctx, 0) else {
        return ctx.fail("Unknown or incomplete command");
    };
    if minutes < 0 {
        return ctx.fail(lang::format(
            lang::ARG_INTEGER_LOW,
            &["0", &minutes.to_string()],
        ));
    }
    set_idle_timeout_minutes(minutes as u64);
    if minutes == 0 {
        Reply::one(text::IDLE_DISABLED)
    } else {
        Reply::one(lang::format(text::IDLE_SET, &[&minutes.to_string()]))
    }
}

fn exec_transfer(ctx: &mut Ctx) -> Reply {
    let Some(host) = text_arg(ctx, 0) else {
        return ctx.fail("Unknown or incomplete command");
    };
    let port = match ctx.args.get(1).map(|arg| &arg.value) {
        Some(Value::Int(port)) => *port,
        Some(_) => return ctx.fail("Unknown or incomplete command"),
        None => 25565,
    };
    if port < 1 {
        return ctx.fail(lang::format(
            lang::ARG_INTEGER_LOW,
            &["1", &port.to_string()],
        ));
    }
    if port > 65535 {
        return ctx.fail(lang::format(
            lang::ARG_INTEGER_BIG,
            &["65535", &port.to_string()],
        ));
    }

    let players = match ctx.args.get(2) {
        Some(arg) => match resolve_targets(ctx.registry, ctx.source, &arg.text, false) {
            Ok(targets) => targets
                .into_iter()
                .filter_map(|target| match target {
                    Target::Online(handle) => Some(handle),
                    Target::Offline { .. } => None,
                })
                .collect::<Vec<_>>(),
            // An empty or unknown selector/name is `argument.entity.notfound.player`
            // (`No player was found`), never `commands.transfer.error.no_players`.
            Err(e) => return ctx.fail(e),
        },
        None => match ctx.player() {
            // With no `players` argument a player transfers themselves; the
            // console has no player, which is `permissions.requires.player`.
            Some(player) => match ctx.registry.by_entity(player.entity_id) {
                Some(handle) => vec![handle],
                None => return ctx.fail(text::NO_PLAYER),
            },
            None => return ctx.fail(text::PLAYER_REQUIRED),
        },
    };
    if players.is_empty() {
        return ctx.fail(text::NO_PLAYER);
    }

    if let Some(unsupported) = players
        .iter()
        .find(|player| player.codec.protocol_id() < 766)
    {
        return ctx.fail(format!(
            "Cannot transfer {}: the client does not support the transfer packet",
            unsupported.name
        ));
    }

    let mut failed = Vec::new();
    for player in &players {
        if !player.transfer(&host, port as u16, ctx.world) {
            failed.push(player.name.clone());
        }
    }
    if !failed.is_empty() {
        return ctx.fail(format!("Could not transfer {}", failed.join(", ")));
    }

    if players.len() == 1 {
        Reply::one(lang::format(
            text::TRANSFER_SINGLE,
            &[&players[0].name, &host, &port.to_string()],
        ))
    } else {
        Reply::one(lang::format(
            text::TRANSFER_MULTIPLE,
            &[&players.len().to_string(), &host, &port.to_string()],
        ))
    }
}

fn int_arg(ctx: &Ctx, index: usize) -> Option<i32> {
    match ctx.args.get(index).map(|arg| &arg.value) {
        Some(Value::Int(value)) => Some(*value),
        _ => None,
    }
}

fn text_arg(ctx: &Ctx, index: usize) -> Option<String> {
    match ctx.args.get(index).map(|arg| &arg.value) {
        Some(Value::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::feedback::FeedbackKind;
    use crate::commands::tree::{execute_feedback_from, execute_feedback_streaming};
    use crate::commands::tree::{ServerKind, Source};
    use crate::players::{PosLook, Registry, SharedRegistry};
    use crate::proto::Conn;
    use crate::protocol::codec_for;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn world() -> crate::session::DemoWorld {
        use aether_api::{FjallStore, World};
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aether-server-cmd-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FjallStore::open(&dir).expect("temp store");
        let seed = crate::seed();
        let generator = crate::worldgen::Generator::load("", "", seed).expect("noise generator");
        let revision = generator.revision();
        let cache = crate::gencache::CacheConfig {
            root: String::new(),
            ..Default::default()
        };
        let generator = crate::gencache::Cached::with_revision(generator, seed, revision, &cache);
        World::new(store, generator)
    }

    fn join(
        registry: &SharedRegistry,
        name: &str,
        id: i32,
        protocol: i32,
    ) -> (Arc<PlayerHandle>, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Conn::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        let peer = listener.accept().unwrap().0;
        let handle = registry
            .join(
                id,
                id as u128,
                name.into(),
                None,
                Vec::new(),
                codec_for(protocol).unwrap(),
                &client,
                PosLook {
                    x: 0.5,
                    y: 64.0,
                    z: 0.5,
                    yaw: 0.0,
                    pitch: 0.0,
                    on_ground: true,
                },
            )
            .unwrap();
        std::mem::forget(client);
        (handle, peer)
    }

    struct Env {
        _guard: std::sync::MutexGuard<'static, ()>,
        _perm: std::sync::MutexGuard<'static, ()>,
        world: crate::session::DemoWorld,
        registry: SharedRegistry,
        tester: Arc<PlayerHandle>,
        bob: Arc<PlayerHandle>,
        _peer_tester: TcpStream,
        _peer_bob: TcpStream,
    }

    fn env(protocol: i32) -> Env {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _perm = crate::permissions::test_lock();
        let mut store = crate::permissions::Store::new();
        store.seed_operators(&["tester".to_string()]);
        crate::permissions::install_store_for_test(store);
        AUTOSAVE.store(true, Ordering::Relaxed);
        set_idle_timeout_minutes(0);
        static IDS: AtomicI32 = AtomicI32::new(10);
        let world = world();
        let registry: SharedRegistry = Arc::new(Registry::default());
        let (tester, peer_tester) = join(
            &registry,
            "tester",
            IDS.fetch_add(2, Ordering::Relaxed),
            protocol,
        );
        let (bob, peer_bob) = join(
            &registry,
            "bob",
            IDS.fetch_add(2, Ordering::Relaxed),
            protocol,
        );
        Env {
            _guard,
            _perm,
            world,
            registry,
            tester,
            bob,
            _peer_tester: peer_tester,
            _peer_bob: peer_bob,
        }
    }

    fn run(env: &Env, source: Source<'_>, text: &str) -> (FeedbackKind, Vec<String>) {
        let feedback = execute_feedback_from(text, source, &env.registry, &env.world)
            .unwrap_or_else(|| panic!("`{text}` was not a command"));
        (feedback.kind, feedback.lines)
    }

    fn ok(env: &Env, text: &str) -> Vec<String> {
        let (kind, lines) = run(env, Source::Player(&env.tester), text);
        assert_eq!(kind, FeedbackKind::Success, "`{text}` => {lines:?}");
        lines
    }

    fn err(env: &Env, text: &str) -> String {
        let (kind, lines) = run(env, Source::Player(&env.tester), text);
        assert_eq!(kind, FeedbackKind::Error, "`{text}` => {lines:?}");
        lines.join("\n")
    }

    fn one(env: &Env, text: &str) -> String {
        let lines = ok(env, text);
        assert_eq!(lines.len(), 1, "`{text}` => {lines:?}");
        lines.into_iter().next().unwrap()
    }

    #[test]
    fn save_on_and_off_toggle_automatic_saving_with_vanilla_wording() {
        let env = env(774);
        assert_eq!(one(&env, "/save-off"), text::SAVE_DISABLED);
        assert!(!autosave_enabled());
        assert_eq!(err(&env, "/save-off"), text::SAVE_ALREADY_OFF);
        assert_eq!(one(&env, "/save-on"), text::SAVE_ENABLED);
        assert!(autosave_enabled());
        assert_eq!(err(&env, "/save-on"), text::SAVE_ALREADY_ON);
    }

    #[test]
    fn save_all_accepts_flush_and_still_saves_when_auto_is_off() {
        let env = env(774);
        // Vanilla reports the start first and the result second; the body can
        // only return both together, so the order is what is asserted.
        let saved = vec![text::SAVING.to_string(), text::SAVED.to_string()];
        assert_eq!(ok(&env, "/save-all"), saved);
        assert_eq!(ok(&env, "/save-all flush"), saved);
        assert_eq!(one(&env, "/save-off"), text::SAVE_DISABLED);
        assert_eq!(ok(&env, "/save-all"), saved);
        // Vanilla's save-all writes the online players too, not only chunks.
        assert!(
            env.world
                .get_meta(&crate::game::player::key(env.tester.uuid))
                .unwrap()
                .is_some(),
            "the player state was saved"
        );
    }

    #[test]
    fn save_all_reports_the_start_before_it_writes_and_the_result_after() {
        let env = env(774);
        let key = crate::game::player::key(env.tester.uuid);
        assert!(env.world.get_meta(&key).unwrap().is_none());
        let mut progress = Vec::new();
        let mut saved_not_yet_written = None;
        let feedback = execute_feedback_streaming(
            "/save-all",
            Source::Player(&env.tester),
            &env.registry,
            &env.world,
            &mut |line| {
                if line == text::SAVING {
                    // The progress line must arrive before the player record
                    // (written by the save) exists.
                    saved_not_yet_written = Some(env.world.get_meta(&key).unwrap().is_none());
                }
                progress.push(line.to_string());
            },
        )
        .expect("a command");
        assert_eq!(
            saved_not_yet_written,
            Some(true),
            "SAVING precedes the write"
        );
        assert_eq!(progress, vec![text::SAVING.to_string()]);
        assert_eq!(feedback.kind, FeedbackKind::Success);
        assert_eq!(feedback.lines, vec![text::SAVED.to_string()]);
        assert!(env.world.get_meta(&key).unwrap().is_some());
    }

    #[test]
    fn two_saves_in_flight_do_not_corrupt_the_world() {
        let env = env(774);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    save_all(&env.world, &env.registry).expect("a concurrent save succeeds");
                });
                scope.spawn(|| {
                    save_players_and_world(&env.world, &env.registry.snapshot())
                        .expect("a shutdown-style save succeeds");
                });
            }
        });
    }

    #[test]
    fn list_is_vanilla_for_no_players_and_for_names_or_uuids() {
        let env = env(774);
        let empty: SharedRegistry = Arc::new(Registry::default());
        let feedback = execute_feedback_from(
            "/list",
            Source::Server(ServerKind::Console),
            &empty,
            &env.world,
        )
        .expect("a command");
        assert_eq!(
            feedback.lines,
            vec![format!(
                "There are 0 of a max of {} players online: ",
                crate::max_players()
            )],
            "empty registry lines differ"
        );

        assert_eq!(
            one(&env, "/list"),
            format!(
                "There are 2 of a max of {} players online: tester, bob",
                crate::max_players()
            )
        );
        let uuids = one(&env, "/list uuids");
        assert!(
            uuids.contains(&format!(
                "bob ({})",
                crate::auth::api::hyphenated_uuid(env.bob.uuid)
            )),
            "{uuids}"
        );
        assert!(
            uuids.contains(&format!(
                "tester ({})",
                crate::auth::api::hyphenated_uuid(env.tester.uuid)
            )),
            "{uuids}"
        );
    }

    #[test]
    fn seed_uses_the_config_seed_and_takes_no_argument() {
        let env = env(774);
        // 1.21.11 `commands.json` declares `/seed` with no argument; the
        // brief's `[<world>]` form is not in the release.
        assert_eq!(one(&env, "/seed"), format!("Seed: [{}]", crate::seed()));
        assert_eq!(
            err(&env, "/seed overworld"),
            "Incorrect argument for command"
        );
        assert_eq!(
            err(&env, "/seed the_nether"),
            "Incorrect argument for command"
        );
    }

    #[test]
    fn version_prints_the_1_21_11_header_and_nine_fields() {
        let env = env(774);
        assert_eq!(
            ok(&env, "/version"),
            vec![
                "Server version info:".to_string(),
                format!("id = {}", env!("CARGO_PKG_VERSION")),
                format!("name = {}", text::AETHER_NAME),
                format!("data = {}", text::AETHER_DATA),
                format!("series = {}", text::AETHER_SERIES),
                format!(
                    "protocol = {} ({})",
                    text::AETHER_PROTOCOL,
                    text::AETHER_PROTOCOL_HEX
                ),
                format!("build_time = {}", text::AETHER_BUILD_TIME),
                format!("pack_resource = {}", text::AETHER_PACK_RESOURCE),
                format!("pack_data = {}", text::AETHER_PACK_DATA),
                format!("stable = {}", text::AETHER_STABLE),
            ]
        );
    }

    #[test]
    fn idle_timeout_sets_disables_and_rejects_bad_values() {
        let env = env(774);
        assert_eq!(
            one(&env, "/setidletimeout 5"),
            "The player idle timeout is now 5 minute(s)"
        );
        assert_eq!(idle_timeout_minutes(), 5);
        assert_eq!(idle_timeout(), Some(Duration::from_secs(300)));
        assert_eq!(one(&env, "/setidletimeout 0"), text::IDLE_DISABLED);
        assert_eq!(idle_timeout(), None);
        assert_eq!(
            err(&env, "/setidletimeout -1"),
            "Integer must not be less than 0: found -1"
        );
        assert_eq!(err(&env, "/setidletimeout x"), "Invalid integer 'x'");
    }

    #[test]
    fn transfer_sends_players_and_reports_clients_that_cannot_transfer() {
        let old = env(763);
        assert!(
            err(&old, "/transfer example.com").contains("does not support"),
            "a pre-1.20.5 client is reported"
        );
        assert!(old.tester.disconnected().is_none());
        drop(old);

        let env = env(774);
        assert_eq!(
            one(&env, "/transfer example.com"),
            "Transferring tester to example.com:25565"
        );
        assert!(
            wait_disconnected(&env.tester),
            "the transferred player's connection is closed"
        );
        assert_eq!(
            env.tester.disconnected().as_deref(),
            Some("Transferred to another server")
        );
        assert_eq!(
            one(&env, "/transfer example.com 25566 bob"),
            "Transferring bob to example.com:25566"
        );
        assert!(wait_disconnected(&env.bob));
        assert_eq!(
            err(&env, "/transfer example.com 0"),
            "Integer must not be less than 1: found 0"
        );
        assert_eq!(
            err(&env, "/transfer example.com 70000"),
            "Integer must not be more than 65535: found 70000"
        );
        // The console has no player to default to; vanilla's
        // `permissions.requires.player` line.
        let (kind, lines) = run(
            &env,
            Source::Server(ServerKind::Console),
            "/transfer example.com",
        );
        assert_eq!(kind, FeedbackKind::Error);
        assert_eq!(lines, vec![text::PLAYER_REQUIRED.to_string()]);
    }

    #[test]
    fn transfer_accepts_a_selector_and_reports_the_group_size() {
        let env = env(774);
        assert_eq!(
            one(&env, "/transfer example.com 25565 @a"),
            "Transferring 2 players to example.com:25565"
        );
        assert!(wait_disconnected(&env.tester));
        assert!(wait_disconnected(&env.bob));
    }

    #[test]
    fn transfer_with_an_empty_or_unknown_target_says_no_player_was_found() {
        let env = env(774);
        assert_eq!(
            err(&env, "/transfer example.com 25565 @a[distance=1000..]"),
            text::NO_PLAYER
        );
        assert_eq!(
            err(&env, "/transfer example.com 25565 NoSuchPlayer"),
            text::NO_PLAYER
        );
        // The argument is `EntityArgument.players`; a non-player selector is
        // refused, and a type=player narrowing is accepted.
        assert_eq!(
            err(&env, "/transfer example.com 25565 @e[type=minecraft:cow]"),
            "Only players may be affected by this command, but the provided selector includes entities"
        );
        assert_eq!(
            one(&env, "/transfer example.com 25565 @a[name=tester]"),
            "Transferring tester to example.com:25565"
        );
        assert!(wait_disconnected(&env.tester));
    }

    #[test]
    fn publish_is_absent_from_the_dedicated_tree() {
        let env = env(774);
        // 1.21.11's dedicated server does not register the singleplayer
        // integrated-server command: both forms are an unknown command.
        assert!(tree::tree().find_command("publish").is_none());
        assert_eq!(err(&env, "/publish"), "Unknown or incomplete command");
        assert_eq!(
            err(&env, "/publish true creative 25565"),
            "Unknown or incomplete command"
        );
    }

    #[test]
    fn permission_levels_match_the_table() {
        let env = env(774);
        let bob = Source::Player(&env.bob);
        for line in [
            "/stop",
            "/save-all",
            "/save-on",
            "/save-off",
            "/seed",
            "/version",
            "/setidletimeout 1",
            "/transfer example.com",
        ] {
            let (kind, lines) = run(&env, bob, line);
            assert_eq!(kind, FeedbackKind::Error, "{line} => {lines:?}");
            assert_eq!(lines[0], "You do not have permission to run this command.");
        }
        // `/list` is level 0 and visible to everyone.
        let (kind, _) = run(&env, bob, "/list");
        assert_eq!(kind, FeedbackKind::Success);
    }

    #[test]
    fn a_level_three_operator_can_manage_players_but_not_own_the_server() {
        let env = env(774);
        let mut store = crate::permissions::Store::new();
        store.seed_operators(&["tester".to_string()]);
        store
            .create_group("level3", crate::permissions::Level::new(3))
            .unwrap();
        store.add_member("level3", "bob", None).unwrap();
        crate::permissions::install_store_for_test(store);

        for line in [
            "/seed",
            "/version",
            "/setidletimeout 1",
            "/transfer example.com",
        ] {
            let (kind, lines) = run(&env, Source::Player(&env.bob), line);
            assert_eq!(kind, FeedbackKind::Success, "{line} => {lines:?}");
        }
        for line in ["/stop", "/save-all"] {
            let (kind, lines) = run(&env, Source::Player(&env.bob), line);
            assert_eq!(kind, FeedbackKind::Error, "{line} => {lines:?}");
            assert_eq!(lines[0], "You do not have permission to run this command.");
        }
    }

    #[test]
    fn stop_requests_a_clean_shutdown_for_an_owner() {
        let env = env(774);
        let _stop = crate::ops::stop_test_lock();
        crate::ops::clear_stop();
        assert_eq!(one(&env, "/stop"), "Stopping the server");
        assert!(crate::ops::stopping());
        crate::ops::clear_stop();
    }

    fn wait_disconnected(player: &PlayerHandle) -> bool {
        for _ in 0..100 {
            if player.disconnected().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}
