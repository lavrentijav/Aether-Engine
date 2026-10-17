//! The `wasm32-wasip2` guest.
//!
//! This module is compiled only for the component target (see `lib.rs`); the
//! pure model and engine build for the host so they can be tested. Everything
//! here is glue: it adapts the SDK services to the [`History`], [`Store`] and
//! [`PermissionCheck`] traits the engine uses, and keeps the one engine
//! instance behind a mutex because the frozen export set is made of plain
//! functions.
//!
//! Built with `make` in this directory; see the README and the Makefile.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

wit_bindgen::generate!({
    world: "aether:sdk-client/dependent",
    generate_all,
    path: [
        "../../crates/aether-plugin/wit",
        "../../crates/aether-plugin/wit-sidebar",
        "../../crates/aether-plugin/wit-sdk",
        "../../crates/aether-plugin/wit-menus",
        "../../crates/aether-plugin-sdk/wit",
    ],
});

// Keep the manifest's `requires` and this list from drifting.
use aether_plugin_sdk::requires;
requires!(
    "sdk.commands@0.1",
    "sdk.history@0.1",
    "sdk.permissions@0.1",
    "sdk.players@0.1",
    "sdk.scheduler@0.1"
);

use aether::plugin::host;
use aether::plugin::types::LogLevel;
use aether::sdk::{commands, config, history, permissions, players, scheduler};

use crate::engine::{
    CommandRequest, History, HistoryPage, JournalEvent, PermissionCheck, StatsEngine, Store,
};
use crate::model::{PlayerId, TICKS_PER_SECOND};

/// The default refresh period, in seconds (30 s at 20 tps).
const DEFAULT_REFRESH_SECONDS: u64 = 30;

/// The one engine, and the scheduler timer id of the refresh, shared by the
/// plain-function exports.
static ENGINE: Mutex<Option<StatsEngine>> = Mutex::new(None);
static REFRESH_ID: AtomicU64 = AtomicU64::new(0);
static LATEST_TICK: AtomicU64 = AtomicU64::new(0);
/// The live session entity id → UUID, so a leave folds playtime even though
/// the server has already dropped the player from its registry (the host
/// cannot resolve a UUID for a player that has left).
static SESSIONS: LazyLock<Mutex<HashMap<u64, PlayerId>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The plugin's KV namespace, over the raw `storage.kv` host import.
struct KvStore;

impl Store for KvStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        host::kv_get(key)
    }
    fn put(&self, key: &str, value: &[u8]) {
        host::kv_put(key, value);
    }
}

/// The PA2 history service, mapped onto the engine's minimal event type.
struct SdkHistory;

impl History for SdkHistory {
    fn player_events(&self, id: PlayerId, before: Option<u64>, limit: u32) -> HistoryPage {
        let page = history::player_events(id.low, id.high, before, limit);
        HistoryPage {
            events: page
                .events
                .into_iter()
                .map(|event| JournalEvent {
                    seq: event.seq,
                    kind: event.kind,
                    from_block: event.from_block,
                    to_block: event.to_block,
                })
                .collect(),
            next: page.next,
        }
    }
}

/// The permission service, bound to the player making the request.
struct SdkPerms {
    player: Player,
}

impl PermissionCheck for SdkPerms {
    fn has(&self, node: &str) -> bool {
        permissions::check(&self.player, node)
    }
}

struct Stats;

/// The caller's identity, or an error line if the host cannot resolve it.
fn caller_id(player: &Player) -> Result<PlayerId, String> {
    players::uuid(player)
        .map(|(low, high)| PlayerId::new(low, high))
        .ok_or_else(|| "Stats are unavailable: your account id could not be resolved.".to_string())
}

/// One root command spec, registered in the core registry through the SDK.
fn root_spec(
    name: &str,
    node: &str,
    usage: &str,
    args: Vec<commands::Arg>,
) -> commands::CommandSpec {
    commands::CommandSpec {
        name: name.to_string(),
        parent: None,
        permission: Some(node.to_string()),
        op: 0,
        usage: usage.to_string(),
        description: "Player stats and leaderboards.".to_string(),
        args,
    }
}

fn arg(name: &str, kind: commands::ArgKind, required: bool) -> commands::Arg {
    commands::Arg {
        name: name.to_string(),
        kind,
        required,
        choices: Vec::new(),
        help: String::new(),
    }
}

impl Guest for Stats {
    fn init(_config: String) -> Result<ManifestAck, String> {
        commands::register(&root_spec(
            "stats",
            "stats.stats",
            "/stats [<player>]",
            vec![arg("player", commands::ArgKind::Player, false)],
        ))?;
        commands::register(&root_spec(
            "leaderboard",
            "stats.leaderboard",
            "/leaderboard <stat> [<page>]",
            vec![
                arg("stat", commands::ArgKind::Word, true),
                arg("page", commands::ArgKind::Integer, false),
            ],
        ))?;
        commands::register(&root_spec(
            "lb",
            "stats.leaderboard",
            "/lb <stat> [<page>]",
            vec![
                arg("stat", commands::ArgKind::Word, true),
                arg("page", commands::ArgKind::Integer, false),
            ],
        ))?;
        commands::register(&root_spec(
            "playtime",
            "stats.playtime",
            "/playtime [<player>|top] [<page>]",
            vec![
                arg("target", commands::ArgKind::Word, false),
                arg("page", commands::ArgKind::Integer, false),
            ],
        ))?;

        // The aggregate refresh, driven by the SDK scheduler (P1). Polled in
        // `on_tick`; the returned id is the only timer this plugin owns. The
        // period is operator-tunable (`[plugins.stats] refresh_seconds`),
        // clamped so a typo cannot turn the refresh into a hot path.
        let refresh_seconds = config::value("refresh_seconds")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_REFRESH_SECONDS)
            .clamp(1, 3600);
        REFRESH_ID.store(
            scheduler::every(refresh_seconds * TICKS_PER_SECOND),
            Ordering::Relaxed,
        );

        let engine = StatsEngine::load(&KvStore);
        *ENGINE.lock().unwrap() = Some(engine);
        host::log(LogLevel::Info, "stats plugin ready");
        Ok(ManifestAck {
            id: "stats".to_string(),
        })
    }

    fn shutdown() {
        // Fold the live sessions in before the process goes away.
        let store = KvStore;
        if let Some(engine) = ENGINE.lock().unwrap().as_mut() {
            engine.persist(&store);
        }
    }

    fn on_command(p: Player, name: String, args: Vec<String>) -> CommandResult {
        let id = match caller_id(&p) {
            Ok(id) => id,
            Err(line) => {
                return CommandResult {
                    handled: true,
                    lines: vec![line],
                };
            }
        };
        let perms = SdkPerms { player: p.clone() };
        let request = CommandRequest {
            caller: id,
            caller_name: &p.name,
            args: &args,
            perms: &perms,
        };
        let now = LATEST_TICK.load(Ordering::Relaxed);
        let lines = match ENGINE.lock().unwrap().as_ref() {
            Some(engine) => engine.handle(&name, &request, now),
            None => Vec::new(),
        };
        CommandResult {
            handled: !lines.is_empty(),
            lines,
        }
    }

    fn after_player_join(p: Player) {
        let Ok(id) = caller_id(&p) else {
            return;
        };
        // Remember the pairing: the leave event arrives after the server has
        // removed the player from its registry, so the UUID must be captured
        // now, while the player is still known.
        SESSIONS.lock().unwrap().insert(p.id, id);
        let store = KvStore;
        let now = LATEST_TICK.load(Ordering::Relaxed);
        if let Some(engine) = ENGINE.lock().unwrap().as_mut() {
            engine.on_join(id, &p.name, now, &store);
        }
    }

    fn after_player_leave(p: Player) {
        // Prefer the pairing recorded at join; the registry lookup is a
        // fallback for a session that started before this plugin was loaded.
        // The lock is released before the fallback's host call.
        let known = SESSIONS.lock().unwrap().remove(&p.id);
        let Some(id) = known.or_else(|| caller_id(&p).ok()) else {
            return;
        };
        let store = KvStore;
        let now = LATEST_TICK.load(Ordering::Relaxed);
        if let Some(engine) = ENGINE.lock().unwrap().as_mut() {
            engine.on_leave(id, now, &store);
        }
    }

    fn on_tick(n: u64) {
        let previous = LATEST_TICK.swap(n, Ordering::Relaxed);
        if previous == 0 {
            // The first tick of this process: a player could have joined
            // before it, with an unknown start. Pin those sessions to now so
            // playtime does not count from the epoch.
            let store = KvStore;
            if let Some(engine) = ENGINE.lock().unwrap().as_mut() {
                engine.rebase_open_sessions(0, n, &store);
            }
        }
        let due = scheduler::poll();
        let refresh = REFRESH_ID.load(Ordering::Relaxed);
        if !due.contains(&refresh) {
            return;
        }
        let store = KvStore;
        if let Some(engine) = ENGINE.lock().unwrap().as_mut() {
            engine.refresh(&SdkHistory, n, &store);
        }
    }

    // --- the frozen export set: no-ops --------------------------------

    fn before_block_break(_p: Player, _x: i32, _y: i32, _z: i32, _block: String) -> Decision {
        Decision::Allow
    }
    fn before_block_place(_p: Player, _x: i32, _y: i32, _z: i32, _block: String) -> Decision {
        Decision::Allow
    }
    fn before_use_item(_p: Player, _item: String) -> Decision {
        Decision::Allow
    }
    fn before_attack(_p: Player, _target: u64) -> Decision {
        Decision::Allow
    }
    fn before_chat(_p: Player, _message: String) -> Decision {
        Decision::Allow
    }
    fn before_container_click(_p: Player, _window: u32, _slot: i32) -> Decision {
        Decision::Allow
    }
    fn after_block_break(_p: Player, _x: i32, _y: i32, _z: i32, _block: String) {}
    fn after_entity_death(_entity: u64, _killer: Option<u64>) {}
    fn after_damage(_entity: u64, _amount: f64, _source: String) {}
    fn after_craft(_p: Player, _item: String, _count: u32) {}
    fn after_enchant(_p: Player, _item: String, _level: u32) {}
}

export!(Stats);
