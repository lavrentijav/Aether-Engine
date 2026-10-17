//! The plugin host: engine, store-per-plugin, sandbox limits and the event
//! bus.
//!
//! One `wasmtime::Store` per plugin isolates memories and keeps a fault count
//! with the instance it belongs to. Every host call into a component goes
//! through [`PluginHost`], which checks the manifest subscription first, so an
//! event a plugin did not ask for is never even entered.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::aether::plugin::host::Host as _HostTrait;
use crate::bindings::aether::plugin::types::{CommandResult, Decision, LogLevel, Player};
use crate::bindings::Plugin;
use crate::manifest::{self, ManifestFile, PluginTier};
use crate::native::{NativeContext, NativePlugin};
use crate::service::ServiceRegistry;
use crate::{Error, Result};

/// Host services a plugin may call. The server implements this over the world
/// store and its own log; tests implement it in memory.
pub trait HostBackend: Send + Sync {
    /// `storage.kv`: read `key` from `plugin`'s namespace.
    fn kv_get(&self, plugin: &str, key: &str) -> Option<Vec<u8>>;
    /// `storage.kv`: write `key`.
    fn kv_put(&self, plugin: &str, key: &str, value: &[u8]);
    /// `storage.kv`: remove `key`.
    fn kv_delete(&self, plugin: &str, key: &str);
    /// `storage.kv`: keys beginning with `prefix`.
    fn kv_list(&self, plugin: &str, prefix: &str) -> Vec<String>;
    /// `storage.kv`: read `key`, telling a missing key from a failed read.
    ///
    /// [`Self::kv_get`] flattens both into `None`, which is fine for a cache
    /// but not for protection state: a plugin that cannot tell "no regions
    /// yet" from "the store is unreadable" must fail closed rather than
    /// overwrite the persisted state with an empty one. The default forwards
    /// to `kv_get` (so a test host still works); the server overrides it to
    /// surface the real storage error.
    fn kv_try_get(&self, plugin: &str, key: &str) -> std::result::Result<Option<Vec<u8>>, String> {
        Ok(self.kv_get(plugin, key))
    }
    /// `storage.kv`: write `key`, reporting a failed write.
    ///
    /// The default forwards to [`Self::kv_put`] and succeeds, so a host with
    /// no failure mode (a test) needs no change. A plugin that must not claim
    /// success before the bytes are durable uses this instead.
    fn kv_try_put(&self, plugin: &str, key: &str, value: &[u8]) -> std::result::Result<(), String> {
        self.kv_put(plugin, key, value);
        Ok(())
    }
    /// `log`: a line tagged with `plugin`.
    fn log(&self, plugin: &str, level: LogLevel, message: &str);
    /// `config`: an operator value from `[plugins.<id>]`.
    fn config(&self, plugin: &str, key: &str) -> Option<String>;
    /// `supports`: whether the host offers a named feature.
    fn supports(&self, plugin: &str, feature: &str) -> bool;

    /// Send a chat line to one online player, identified by session entity id.
    ///
    /// Used by the SDK `messages` service. The default does nothing, so a host
    /// (or a test) that has no player access still satisfies the trait; the
    /// server overrides it to reach its player registry.
    fn send_message(&self, _plugin: &str, _player: u64, _text: &str) {}

    /// Broadcast a chat line to every online player.
    fn broadcast(&self, _plugin: &str, _text: &str) {}

    /// Send a plugin-channel (custom payload) message to one player. The
    /// default does nothing; the server encodes it in the player's own codec.
    /// Used by the voice plugin for the Simple Voice Chat and Plasmo Voice
    /// handshake.
    fn send_plugin_message(&self, _plugin: &str, _player: u64, _channel: &str, _data: &[u8]) {}

    /// A player's 128-bit UUID, for a plugin whose protocol (voice chat) keys
    /// everything by UUID rather than the session entity id. The default is
    /// `None`.
    fn player_uuid(&self, _plugin: &str, _player: u64) -> Option<u128> {
        None
    }

    /// A player's current world name and position, for proximity voice. The
    /// default is `None`, which makes every listener "out of range".
    fn player_position(&self, _plugin: &str, _player: u64) -> Option<(String, f64, f64, f64)> {
        None
    }

    /// The item in a player's main hand (a `minecraft:` name), for a plugin
    /// that must tell its own tool from an ordinary block click. The default
    /// is `None`, so a host without inventories answers "nothing held".
    fn held_item(&self, _plugin: &str, _player: u64) -> Option<String> {
        None
    }

    /// Move a player to `(x, y, z)`, searching for a safe spot. The default
    /// refuses, so a host with no world (a test, a stripped build) does not
    /// pretend the move happened. The server overrides it to front the core
    /// teleport service (CMD-31).
    fn teleport_player(
        &self,
        _plugin: &str,
        _player: u64,
        _x: f64,
        _y: f64,
        _z: f64,
        _cause: crate::sdk::TeleportCause,
    ) -> std::result::Result<(), String> {
        Err("teleport is not supported by this host".to_string())
    }

    /// Disconnect a player with `reason`. The default does nothing.
    fn disconnect_player(&self, _plugin: &str, _player: u64, _reason: &str) {}

    /// The online players, for the SDK `players` service. The default is empty.
    fn online_players(&self, _plugin: &str) -> Vec<Player> {
        Vec::new()
    }

    /// A change counter for `[plugins.<id>]`, so a dependent can detect a
    /// reload cheaply. The default is constant (no reloads).
    fn config_revision(&self, _plugin: &str) -> u64 {
        0
    }

    /// Register a plugin command in the core command registry, so it is
    /// dispatched and permission-checked the same way a core command is.
    ///
    /// The SDK's command builder calls this; a host without a command registry
    /// (a test, a stripped build) accepts and remembers nothing. `Err` carries
    /// the core's reason — a core name, or a name already taken.
    fn register_command(
        &self,
        _plugin: &str,
        _name: &str,
        _node: &str,
        _op: u8,
    ) -> std::result::Result<(), String> {
        Ok(())
    }

    /// The capabilities the host granted `plugin` (its effective grant).
    ///
    /// The SDK's service layer reads this to gate its own privileged helpers
    /// (`permissions.admin`, message sends) in **one** place, keyed by the
    /// caller id, so the gate applies to native dependents as well as WASM
    /// ones. The default is empty — deny — matching the host's own policy.
    fn capabilities(&self, _plugin: &str) -> Vec<String> {
        Vec::new()
    }

    /// Play a named sound to one player, for a menu's `[sound]` action. The
    /// default does nothing, so a host with no world (a test) still satisfies
    /// the trait; the server plays it through the core sound event.
    fn play_sound(&self, _plugin: &str, _player: u64, _name: &str) {}

    /// Give `count` of `item` (a `minecraft:` name) to a player, dropping
    /// whatever does not fit at their feet, for a menu's `[give]` action.
    /// Returns how many could **not** be handed over. The default gives
    /// nothing and reports all `count` as leftover, so a host without an
    /// inventory model (a test) fails closed rather than pretending.
    fn give_item(&self, _plugin: &str, _player: u64, _item: &str, count: u32) -> u64 {
        count as u64
    }

    // --- virtual menus (the core window API) --------------------------------
    //
    // The native `plugins/menus` host calls these to draw server-invented
    // windows. The core guarantees a click on one never moves an item; the
    // backend only translates the neutral menu item into a window slot. A host
    // with no window support (a test, a stripped build) answers the neutral
    // value, so the menus plugin degrades rather than panics.

    /// Open a virtual menu window for `player` (a session entity id). `items`
    /// not in `0..layout.slot_count()` are ignored. Returns whether a window
    /// was opened.
    fn open_menu(
        &self,
        _plugin: &str,
        _player: u64,
        _menu: u64,
        _title: &str,
        _layout: crate::menus::MenuLayout,
        _items: &[crate::menus::MenuItem],
    ) -> bool {
        false
    }

    /// Replace one slot of an open menu, or clear it with `None`.
    fn set_menu_slot(
        &self,
        _plugin: &str,
        _player: u64,
        _menu: u64,
        _slot: u32,
        _item: Option<&crate::menus::MenuItem>,
    ) -> bool {
        false
    }

    /// Close an open menu.
    fn close_menu(&self, _plugin: &str, _player: u64, _menu: u64) {}

    /// Run `command` (without the leading slash) for a player, or on the
    /// console when `player` is `None`; returns the reply lines. Used by the
    /// menus `[command]`/`[console]` actions.
    fn run_command(&self, _plugin: &str, _player: Option<u64>, _command: &str) -> Vec<String> {
        Vec::new()
    }

    /// `permissions`: whether the online player with session id `id` holds
    /// `node` for a command needing `op`.
    ///
    /// Used by the SDK `permissions` service. The default is **deny** — a host
    /// (or a test) with no permission core fails closed, which is the safe
    /// answer, and the server overrides it to front [`crate::permissions`].
    fn permission_check(&self, _plugin: &str, _id: u64, _node: &str, _op: u8) -> bool {
        false
    }

    /// `permissions`: the online player's effective level. The default is 0.
    fn permission_level(&self, _plugin: &str, _id: u64) -> u8 {
        0
    }

    /// `permissions`: the groups the online player belongs to. The default is
    /// empty.
    fn permission_groups(&self, _plugin: &str, _id: u64) -> Vec<String> {
        Vec::new()
    }

    /// `permissions`: a meta value (prefix/suffix/weight) for the online
    /// player. The default is `None`; meta is CMD-02.
    fn permission_meta(&self, _plugin: &str, _id: u64, _key: &str) -> Option<String> {
        None
    }

    /// `permissions`: grant or deny `node` to `subject`. The default refuses:
    /// the per-player write model is CMD-02, and a host without it must not
    /// pretend the write happened.
    fn permission_grant(
        &self,
        _plugin: &str,
        _subject: &str,
        _node: &str,
        _value: bool,
    ) -> std::result::Result<(), String> {
        Err("the permission write model (CMD-02) is not available yet".to_string())
    }

    /// `permissions`: remove an explicit `node` from `subject`. The default
    /// refuses, as [`Self::permission_grant`].
    fn permission_revoke(
        &self,
        _plugin: &str,
        _subject: &str,
        _node: &str,
    ) -> std::result::Result<(), String> {
        Err("the permission write model (CMD-02) is not available yet".to_string())
    }

    // --- History API (PA2) -------------------------------------------------
    //
    // Read-only journal queries behind the SDK `history` service. The defaults
    // are the neutral answer — an empty page, "not original", an empty chain —
    // so a host without a journal (a test, a stripped build) still satisfies
    // the trait and a dependent sees an empty history rather than a trap. The
    // server overrides every one of these over its world journal.

    /// The full timeline of the block at `(x, y, z)`, newest first.
    fn history_block(
        &self,
        _plugin: &str,
        _x: i32,
        _y: i32,
        _z: i32,
        _before: Option<u64>,
        _limit: u32,
    ) -> crate::sdk::HistoryPage {
        crate::sdk::empty_history_page()
    }

    /// Whether nobody has ever changed the block at `(x, y, z)`.
    fn history_worldgen_original(&self, _plugin: &str, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    /// Events matching `window`, newest first.
    fn history_events(
        &self,
        _plugin: &str,
        _window: crate::sdk::HistoryWindow,
        _before: Option<u64>,
        _limit: u32,
    ) -> crate::sdk::HistoryPage {
        crate::sdk::empty_history_page()
    }

    /// The provenance chain of item `uid`, newest first, paged.
    fn history_item_chain(
        &self,
        _plugin: &str,
        _uid: u64,
        _depth: u32,
        _before: Option<u64>,
        _limit: u32,
    ) -> crate::sdk::HistoryPage {
        crate::sdk::empty_history_page()
    }

    /// Who accessed the container at `(x, y, z)`, newest first.
    fn history_container_log(
        &self,
        _plugin: &str,
        _x: i32,
        _y: i32,
        _z: i32,
        _before: Option<u64>,
        _limit: u32,
    ) -> crate::sdk::HistoryPage {
        crate::sdk::empty_history_page()
    }

    /// One player's event stream, newest first.
    fn history_player_events(
        &self,
        _plugin: &str,
        _actor_low: u64,
        _actor_high: u64,
        _before: Option<u64>,
        _limit: u32,
    ) -> crate::sdk::HistoryPage {
        crate::sdk::empty_history_page()
    }

    /// Replace a player's sidebar. The core diffs the board against what the
    /// player already shows and writes only the packets that changed, so a
    /// caller may hand over the whole board every update. The default does
    /// nothing, so a host (or a test) without a scoreboard still satisfies the
    /// trait.
    fn set_sidebar(&self, _plugin: &str, _player: u64, _board: &crate::sidebar::SidebarBoard) {}

    /// Hide a player's sidebar. The default does nothing.
    fn clear_sidebar(&self, _plugin: &str, _player: u64) {}

    /// Set a player's tab-list header and footer (plain text, `§` codes). The
    /// core resends only when the pair changes. The default does nothing.
    fn set_tab_header_footer(&self, _plugin: &str, _player: u64, _header: &str, _footer: &str) {}

    /// Whether the vanilla `/scoreboard` model already owns the `sidebar`
    /// display slot for `player` (an objective other than the plugin's set
    /// with `setdisplay sidebar`). The sidebar plugin reads this to honour
    /// `mode = "yield"`. Until CMD-15 lands the core keeps no vanilla
    /// scoreboard, so the default is `false`.
    fn vanilla_sidebar_claimed(&self, _plugin: &str, _player: u64) -> bool {
        false
    }

    /// The extra per-player facts a placeholder can show (position, world,
    /// ping, health, level). The default is `None`: a host that cannot answer
    /// simply leaves those placeholders empty.
    fn player_extra(
        &self,
        _plugin: &str,
        _player: u64,
    ) -> Option<crate::placeholders::PlayerExtra> {
        None
    }

    /// Ticks per second, for `%tps%`. The default is the ideal 20.
    fn server_tps(&self, _plugin: &str) -> f64 {
        20.0
    }

    /// The configured player-slot cap, for `%max%`. The default is the vanilla
    /// default.
    fn max_players(&self, _plugin: &str) -> u32 {
        20
    }

    /// The world time of day, in ticks (`0..24000`), for the world clock. The
    /// default is sunrise.
    fn world_time(&self, _plugin: &str) -> i64 {
        0
    }

    // --- Staff accountability (the native `staff` plugin) ------------------
    //
    // The core owns the *mechanism* — who is hidden from whom, the spectate
    // snapshot, the anti-xray override — because another player's client and
    // the render path need the answer without a plugin call. These hooks are
    // how the plugin sets it and reads it back. Every default is neutral, so a
    // host without the mechanism satisfies the trait.

    /// Hide or reveal `player` from other players (vanish).
    fn staff_set_vanished(&self, _plugin: &str, _player: u64, _vanished: bool) {}

    /// Whether `player` is currently vanished.
    fn staff_is_vanished(&self, _plugin: &str, _player: u64) -> bool {
        false
    }

    /// Grant or revoke `player`'s ability to see vanished players
    /// (`aether.staff.vanish.see`).
    fn staff_set_sees_vanished(&self, _plugin: &str, _player: u64, _see: bool) {}

    /// Whether `player` is in spectate mode.
    fn staff_is_spectating(&self, _plugin: &str, _player: u64) -> bool {
        false
    }

    /// Enter spectate for `player`, optionally moving onto `target`'s camera.
    /// `Err` explains why it could not (already spectating, no snapshot, …).
    fn staff_enter_spectate(
        &self,
        _plugin: &str,
        _player: u64,
        _target: Option<u64>,
    ) -> std::result::Result<(), String> {
        Err("spectate is not available on this host".into())
    }

    /// Leave spectate, restoring the saved location, mode, inventory, health,
    /// hunger, effects and flight.
    fn staff_leave_spectate(&self, _plugin: &str, _player: u64) -> std::result::Result<(), String> {
        Err("spectate is not available on this host".into())
    }

    /// Set `player`'s anti-xray level (`off`, `engine`, `culling`, or a margin
    /// in blocks), re-sending the columns they hold.
    fn staff_set_xray_level(
        &self,
        _plugin: &str,
        _player: u64,
        _level: &str,
    ) -> std::result::Result<(), String> {
        Err("anti-xray control is not available on this host".into())
    }

    /// Show a line above `player`'s hotbar.
    fn staff_actionbar(&self, _plugin: &str, _player: u64, _text: &str) {}

    /// Append a staff-accountability event to the world journal: who did what.
    fn staff_journal(&self, _plugin: &str, _actor: u64, _action: &str, _detail: &str) {}

    // --- Regions and claims (the native `regions` / `claims` plugins) -------
    //
    // The core owns the index and the protection check (CMD-29a/29b); these
    // hooks are how a plugin fills it. The host stamps the caller id as the
    // region source, so a plugin may only replace or remove a region it owns.
    // The defaults are neutral: a host with no region store accepts the write
    // and reports no regions, which keeps a test backend small.

    /// Add or replace a region owned by `plugin`.
    ///
    /// The ownership test and the write are atomic in the host: replacing a
    /// core-created region (`source: None`) or another plugin's region is
    /// refused with a [`crate::RegionWriteError`].
    fn region_upsert(
        &self,
        _plugin: &str,
        _region: &crate::region::RegionInfo,
    ) -> std::result::Result<(), crate::RegionWriteError> {
        Ok(())
    }

    /// Insert a **fresh** region owned by `plugin`. The id must be free — any
    /// occupied id, including one this plugin already owns, is refused with
    /// [`crate::RegionWriteError::IdTaken`] — and the box must not overlap a
    /// region from the same plugin.
    ///
    /// The id and overlap tests and the write share the store's writer lock, so
    /// two competing claim attempts cannot both win and a second insert can
    /// never silently replace the first. Overlapping a region from another
    /// plugin is allowed (the core's priority chain decides the protection
    /// verdict). An idempotent startup replay uses [`HostBackend::region_upsert`]
    /// instead.
    fn region_insert(
        &self,
        plugin: &str,
        region: &crate::region::RegionInfo,
    ) -> std::result::Result<(), crate::RegionWriteError> {
        self.region_upsert(plugin, region)
    }

    /// Allocate a globally unique region-id serial, at least `floor`.
    ///
    /// The host allocator is atomic across plugin instances and threads, so two
    /// instances that would each start counting at one cannot pick the same id.
    /// The default is the process-wide allocator in [`crate::region`].
    fn region_allocate_serial(&self, _plugin: &str, floor: u64) -> u64 {
        crate::region::allocate_serial(floor)
    }

    /// Remove a region owned by `plugin`. A missing id, a core-owned region or
    /// another plugin's region is a no-op.
    fn region_remove(&self, _plugin: &str, _id: u64) {}

    /// The region with `id`, if it exists.
    fn region_get(&self, _plugin: &str, _id: u64) -> Option<crate::region::RegionInfo> {
        None
    }

    /// Every region in `world` that overlaps the inclusive box `min..=max`.
    ///
    /// Used to refuse a claim that would cover an existing claim and to
    /// inspect what is already at a position. The host returns regions from
    /// every plugin, with [`crate::region::RegionInfo::source`] filled in.
    fn regions_overlapping(
        &self,
        _plugin: &str,
        _world: &str,
        _min: [i32; 3],
        _max: [i32; 3],
    ) -> Vec<crate::region::RegionInfo> {
        Vec::new()
    }
}

/// Sandbox and scheduling limits for the whole host.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Per-call time budget, in milliseconds. Enforced with epoch interruption.
    pub budget_ms: u64,
    /// Shared time budget, in milliseconds, that **all** plugins may spend
    /// *running* across one server tick. Once it is exhausted no further
    /// plugin is called until the next tick, so a plugin (or many) cannot
    /// stall the tick even when each individual call stays inside
    /// `budget_ms`.
    ///
    /// It measures time actually spent inside plugin calls, reset at the
    /// start of each tick; it is not a wall-clock deadline. A tick is 50 ms
    /// but a `before-*` event may fire near its end, and the time between
    /// plugin calls is the server's, not the plugins', so neither must count
    /// against it.
    pub tick_budget_ms: u64,
    /// Per-plugin linear-memory cap, in mebibytes.
    pub memory_mib: u64,
    /// How many times a plugin may trap before the host disables it.
    pub max_faults: u32,
    /// Fuel granted to instantiation and `init`. Init runs once and does
    /// first-time work a timed call must not: the epoch deadline is not
    /// appropriate for it, so it is bounded by fuel instead.
    pub init_fuel: u64,
    /// Built-in command names a plugin may never handle, set by the embedder.
    /// The host refuses to route any of these to a plugin, so a plugin that
    /// registers `give` or `tp` cannot shadow the built-in and its operator
    /// check. The embedder supplies the names because only it knows them.
    pub reserved_commands: Vec<String>,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            budget_ms: 5,
            tick_budget_ms: 25,
            memory_mib: 64,
            max_faults: 5,
            init_fuel: 100_000_000,
            reserved_commands: Vec::new(),
        }
    }
}

/// The store data every import and export call sees.
///
/// `pub(crate)` because the SDK import bridge in [`crate::sdk`] implements the
/// generated `aether:sdk` host traits on it, reading the caller's id and
/// capabilities.
pub(crate) struct StoreData {
    pub(crate) plugin: String,
    pub(crate) capabilities: Vec<String>,
    pub(crate) config: BTreeMap<String, String>,
    pub(crate) backend: Arc<dyn HostBackend>,
    /// The shared service registry, so a WASM component can call the native
    /// SDK services through the generated `aether:sdk` imports.
    pub(crate) services: Arc<ServiceRegistry>,
    limits: StoreLimits,
    /// A context that grants the component nothing: no filesystem preopens,
    /// no environment, no arguments, closed stdin and a sink for stdout and
    /// stderr. A `wasm32-wasip2` Rust component imports the WASI 0.2 surface
    /// for its standard library, so the linker must satisfy it; the component
    /// still cannot reach the host through it.
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for StoreData {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl StoreData {
    /// Build a bare store for unit-testing the import bridges without a WASM
    /// component or an engine. Not part of the public surface.
    #[cfg(test)]
    pub(crate) fn for_test(
        plugin: &str,
        capabilities: Vec<String>,
        services: Arc<ServiceRegistry>,
        backend: Arc<dyn HostBackend>,
    ) -> StoreData {
        StoreData {
            plugin: plugin.to_string(),
            capabilities,
            config: BTreeMap::new(),
            backend,
            services,
            limits: StoreLimitsBuilder::new().build(),
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        }
    }

    pub(crate) fn allowed(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// Refuse an ungranted call: log the denial and answer with the neutral
    /// value. Panicking would let a manifest typo take the server down, which
    /// is exactly what a sandbox is for.
    pub(crate) fn deny(&self, capability: &str, what: &str) {
        self.backend.log(
            &self.plugin,
            LogLevel::Warn,
            &format!("capability `{capability}` not granted; refused {what}"),
        );
    }

    /// Log a warning tagged with this plugin's id.
    pub(crate) fn warn(&self, message: &str) {
        self.backend.log(&self.plugin, LogLevel::Warn, message);
    }
}

impl crate::bindings::aether::plugin::types::Host for StoreData {}

impl _HostTrait for StoreData {
    fn log(&mut self, level: LogLevel, message: String) {
        if !self.allowed("log") {
            return self.deny("log", "log");
        }
        self.backend.log(&self.plugin, level, &message);
    }

    fn kv_get(&mut self, key: String) -> Option<Vec<u8>> {
        if !self.allowed("storage.kv") {
            self.deny("storage.kv", "kv-get");
            return None;
        }
        self.backend.kv_get(&self.plugin, &key)
    }

    fn kv_put(&mut self, key: String, value: Vec<u8>) {
        if !self.allowed("storage.kv") {
            return self.deny("storage.kv", "kv-put");
        }
        self.backend.kv_put(&self.plugin, &key, &value);
    }

    fn kv_delete(&mut self, key: String) {
        if !self.allowed("storage.kv") {
            return self.deny("storage.kv", "kv-delete");
        }
        self.backend.kv_delete(&self.plugin, &key);
    }

    fn kv_list(&mut self, prefix: String) -> Vec<String> {
        if !self.allowed("storage.kv") {
            self.deny("storage.kv", "kv-list");
            return Vec::new();
        }
        self.backend.kv_list(&self.plugin, &prefix)
    }

    fn config(&mut self, key: String) -> Option<String> {
        if !self.allowed("config") {
            self.deny("config", "config");
            return None;
        }
        self.config
            .get(&key)
            .cloned()
            .or_else(|| self.backend.config(&self.plugin, &key))
    }

    fn supports(&mut self, feature: String) -> bool {
        if !self.allowed("supports") {
            self.deny("supports", "supports");
            return false;
        }
        self.backend.supports(&self.plugin, &feature)
    }
}

/// The instantiation behind a loaded plugin: a WASM component in its store, or
/// an in-process native plugin.
enum Kind {
    Wasm {
        store: Store<StoreData>,
        // Boxed: the generated instance bindings are several hundred bytes,
        // and a native plugin's variant is a single pointer, so an unboxed
        // enum would make every plugin entry carry the WASM size.
        bindings: Box<Plugin>,
    },
    Native(Box<dyn NativePlugin>),
}

/// One loaded plugin, either tier. Carries the manifest, the effective
/// capabilities and config (needed to build a native [`NativeContext`]) and the
/// fault/disable bookkeeping shared by both tiers.
struct LoadedPlugin {
    manifest: manifest::Manifest,
    kind: Kind,
    capabilities: Vec<String>,
    config: BTreeMap<String, String>,
    faults: u32,
    disabled: bool,
}

impl LoadedPlugin {
    /// Whether this plugin should receive `event` right now.
    fn receives(&self, event: &str) -> bool {
        !self.disabled && self.manifest.subscribes(event)
    }

    /// Set the epoch deadline for a WASM plugin; a no-op for a native one,
    /// which has no interruptible clock.
    fn set_deadline(&mut self, ms: u64) {
        if let Kind::Wasm { store, .. } = &mut self.kind {
            store.set_epoch_deadline(ms);
        }
    }

    /// Call one of the `before-*` hooks, whichever tier this plugin is.
    #[allow(clippy::too_many_arguments)]
    fn call_before_block_break(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_before_block_break(store, p, x, y, z, block)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_block_break(&mut ctx, p, x, y, z, block))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn call_before_block_place(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_before_block_place(store, p, x, y, z, block)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_block_place(&mut ctx, p, x, y, z, block))
            }
        }
    }

    fn call_before_use_item(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        item: &str,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_before_use_item(store, p, item)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_use_item(&mut ctx, p, item))
            }
        }
    }

    /// A right-click on a block, for the regions selection wand. Native-only:
    /// a WASM component has no export for it, so it always answers `Allow`
    /// (never a trap and never a veto).
    #[allow(clippy::too_many_arguments)]
    fn call_before_block_interact(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { .. } => Ok(Decision::Allow),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_block_interact(&mut ctx, p, x, y, z, block))
            }
        }
    }

    /// The player started mining a block. Native-only, like
    /// `before-block-interact`, and called **before** the core's region check
    /// and before any break: a selection wand needs the click even on an
    /// unbreakable or protected block. Calling it from the break itself would
    /// be too late in survival.
    #[allow(clippy::too_many_arguments)]
    fn call_before_block_dig(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { .. } => Ok(Decision::Allow),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_block_dig(&mut ctx, p, x, y, z, block))
            }
        }
    }

    /// A right-click on a block with an item, for the claims golden shovel.
    /// The frozen `@0.1.0` WIT world has no export for this event, so a WASM
    /// plugin cannot subscribe to it; only native plugins answer.
    #[allow(clippy::too_many_arguments)]
    fn call_before_use_block(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        item: &str,
        block: &str,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            // The frozen `@0.1.0` WIT world has no export for this event, so a
            // WASM plugin cannot subscribe to it; only native plugins answer.
            Kind::Wasm { .. } => Ok(Decision::Allow),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_use_block(&mut ctx, p, x, y, z, item, block))
            }
        }
    }

    fn call_before_attack(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        target: u64,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_before_attack(store, p, target)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_attack(&mut ctx, p, target))
            }
        }
    }

    fn call_before_chat(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        message: &str,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_before_chat(store, p, message)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_chat(&mut ctx, p, message))
            }
        }
    }

    fn call_before_container_click(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        window: u32,
        slot: i32,
    ) -> std::result::Result<Decision, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_before_container_click(store, p, window, slot)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                Ok(n.before_container_click(&mut ctx, p, window, slot))
            }
        }
    }

    fn call_after_player_join(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_after_player_join(store, p)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.after_player_join(&mut ctx, p);
                Ok(())
            }
        }
    }

    fn call_after_player_leave(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_after_player_leave(store, p)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.after_player_leave(&mut ctx, p);
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn call_after_block_break(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_after_block_break(store, p, x, y, z, block)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.after_block_break(&mut ctx, p, x, y, z, block);
                Ok(())
            }
        }
    }

    /// `after-block-place`. Native-only: the frozen WIT world has no export,
    /// so a WASM plugin subscribed to the event is silently skipped (the
    /// manifest still parses, which keeps one manifest valid for both tiers).
    #[allow(clippy::too_many_arguments)]
    fn call_after_block_place(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { .. } => Ok(()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.after_block_place(&mut ctx, p, x, y, z, block);
                Ok(())
            }
        }
    }

    fn call_after_entity_death(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        entity: u64,
        killer: Option<u64>,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_after_entity_death(store, entity, killer)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.after_entity_death(&mut ctx, entity, killer);
                Ok(())
            }
        }
    }

    fn call_after_damage(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        entity: u64,
        amount: f64,
        source: &str,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_after_damage(store, entity, amount, source)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.after_damage(&mut ctx, entity, amount, source);
                Ok(())
            }
        }
    }

    fn call_after_craft(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        item: &str,
        count: u32,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_after_craft(store, p, item, count)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.after_craft(&mut ctx, p, item, count);
                Ok(())
            }
        }
    }

    fn call_after_enchant(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        item: &str,
        level: u32,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_after_enchant(store, p, item, level)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.after_enchant(&mut ctx, p, item, level);
                Ok(())
            }
        }
    }

    fn call_on_tick(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        n: u64,
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => {
                bindings.call_on_tick(store, n).map_err(|e| e.to_string())
            }
            Kind::Native(p) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                p.on_tick(&mut ctx, n);
                Ok(())
            }
        }
    }

    fn call_on_command(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> std::result::Result<CommandResult, String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { store, bindings } => bindings
                .call_on_command(store, p, name, args)
                .map_err(|e| e.to_string()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                let result = n
                    .on_command(&mut ctx, p, name, args)
                    .unwrap_or(CommandResult {
                        handled: false,
                        lines: Vec::new(),
                    });
                Ok(result)
            }
        }
    }

    /// A plugin-channel message. Native plugins subscribed to
    /// `on-plugin-message` are called in order; the frozen WASM world has no
    /// callback, so a WASM plugin is a no-op here.
    fn call_on_plugin_message(
        &mut self,
        services: &Arc<ServiceRegistry>,
        backend: &Arc<dyn HostBackend>,
        p: &Player,
        channel: &str,
        data: &[u8],
    ) -> std::result::Result<(), String> {
        let LoadedPlugin {
            manifest,
            kind,
            capabilities,
            config,
            ..
        } = self;
        match kind {
            Kind::Wasm { .. } => Ok(()),
            Kind::Native(n) => {
                let mut ctx = NativeContext {
                    id: &manifest.id,
                    capabilities,
                    config,
                    services,
                    backend,
                };
                n.on_plugin_message(&mut ctx, p, channel, data);
                Ok(())
            }
        }
    }
}

/// Discover every `plugin.toml` directly under `dir` or one level below it.
///
/// The directory-per-plugin layout (`plugins/hello/plugin.toml`) keeps the
/// component next to its manifest; a `plugin.toml` directly in `dir` is
/// accepted too, for a single-plugin install.
pub fn discover(dir: &Path) -> Vec<ManifestFile> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            let candidate = path.join("plugin.toml");
            if candidate.is_file() {
                match manifest::load(&candidate) {
                    Ok(m) => found.push(m),
                    Err(e) => eprintln!("aether-plugin: {e}"),
                }
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some("plugin.toml") {
            match manifest::load(&path) {
                Ok(m) => found.push(m),
                Err(e) => eprintln!("aether-plugin: {e}"),
            }
        }
    }
    found
}

/// The loaded plugins, ordered, and the engine they share.
pub struct PluginHost {
    engine: Engine,
    config: HostConfig,
    backend: Arc<dyn HostBackend>,
    /// Cross-plugin services, published by native providers (the SDK) and
    /// resolved by dependents before they load and again at call time.
    services: Arc<ServiceRegistry>,
    plugins: Vec<LoadedPlugin>,
    /// Wall-clock time actually spent **inside plugin calls** since the
    /// current tick began. `None` until the first [`on_tick`], so before any
    /// tick there is no tick window to overrun and calls are bounded only by
    /// the per-call budget. Reset to zero at the top of every [`on_tick`].
    ///
    /// Counting consumed time rather than a deadline is what keeps the budget
    /// a property of the plugins, not of the wall clock: a tick's 50 ms pass
    /// between `on_tick` and a later `before-*`, and an idle gap must not
    /// spend the budget. Only time the plugins actually ran counts.
    tick_used: Option<Duration>,
    /// The shared budget as a duration, kept beside the used total.
    tick_budget: Duration,
    /// Whether the background epoch ticker has been started. It starts with
    /// the first plugin; a host that never loads one never spawns a thread.
    ticker_started: bool,
}

impl PluginHost {
    /// Build a host. The epoch ticker that drives the per-call timeout is
    /// started lazily by the first [`add`](PluginHost::add), so a host that
    /// ends up with no plugins spawns no thread and wakes nothing.
    pub fn new(config: HostConfig, backend: Arc<dyn HostBackend>) -> Result<PluginHost> {
        let mut wasm = Config::new();
        wasm.epoch_interruption(true);
        // Init is bounded by fuel rather than by the per-call epoch budget.
        wasm.consume_fuel(true);
        let engine = Engine::new(&wasm)?;
        let tick_budget = Duration::from_millis(config.tick_budget_ms.max(1));
        Ok(PluginHost {
            engine,
            config,
            backend,
            services: Arc::new(ServiceRegistry::new()),
            plugins: Vec::new(),
            tick_used: None,
            tick_budget,
            ticker_started: false,
        })
    }

    /// Start the epoch ticker if it is not already running. It advances the
    /// engine's epoch every millisecond; a call's deadline is a number of
    /// increments away. Idempotent, and only ever called once a plugin exists.
    fn ensure_ticker(&mut self) -> Result<()> {
        if self.ticker_started {
            return Ok(());
        }
        let ticker = self.engine.clone();
        std::thread::Builder::new()
            .name("plugin-epoch".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(1));
                ticker.increment_epoch();
            })
            .map_err(|e| Error::new(format!("cannot start the epoch ticker: {e}")))?;
        self.ticker_started = true;
        Ok(())
    }

    /// Whether the background epoch ticker is running. It starts with the
    /// first plugin loaded, so false here means the host has spawned no
    /// thread and the server can skip installing it at all.
    pub fn has_epoch_ticker(&self) -> bool {
        self.ticker_started
    }

    /// Reset the tick's consumed-time total to zero. Called at the start of
    /// every server tick, before any plugin for that tick runs. It does not
    /// start a clock: idling between plugin calls must not spend the budget.
    fn begin_tick(&mut self) {
        self.tick_used = Some(Duration::ZERO);
    }

    /// The epoch deadline, in milliseconds, for the next plugin call: the
    /// per-call budget, further capped by what is left of the shared tick
    /// budget. **Zero means the tick's budget is spent** and no further plugin
    /// should run this tick.
    fn call_budget_ms(&self) -> u64 {
        let per_call = self.config.budget_ms.max(1);
        match self.tick_used {
            // No tick has run yet: there is no window to spend.
            None => per_call,
            Some(used) => {
                let remaining = self.tick_budget.saturating_sub(used).as_millis() as u64;
                per_call.min(remaining)
            }
        }
    }

    /// Charge a finished plugin call against the tick's consumed time.
    ///
    /// `granted_ms` is the per-call allowance the call was given and `trapped`
    /// whether it errored. A call that trapped on its epoch deadline ran to
    /// that deadline, so it is charged at least its full allowance even if the
    /// stopwatch reads a hair under it; a call that returned is charged exactly
    /// what it took. Idle time is never passed here and so never counts.
    fn charge_tick(&mut self, started: Instant, granted_ms: u64, trapped: bool) {
        let elapsed = started.elapsed();
        let charged = if trapped {
            elapsed.max(Duration::from_millis(granted_ms))
        } else {
            elapsed
        };
        if let Some(used) = self.tick_used.as_mut() {
            *used += charged;
        }
    }

    /// The services this host provides, so a caller can register a native
    /// provider into them before dependents load.
    pub fn services(&self) -> &Arc<ServiceRegistry> {
        &self.services
    }

    /// Refuse to load a plugin whose `requires` are not all satisfied.
    ///
    /// A missing service is a configuration error with a name attached, not a
    /// runtime surprise: the dependent is not added and the error says which
    /// service and points at the provider.
    fn check_requires(&self, manifest: &manifest::Manifest) -> Result<()> {
        let required = manifest.required_services().map_err(Error::new)?;
        for service in required {
            if !self.services.contains(&service) {
                return Err(Error::new(format!(
                    "plugin `{}` requires service `{service}`, which is not registered \
                     (is the provider plugin enabled?)",
                    manifest.id
                )));
            }
        }
        Ok(())
    }

    /// Refuse a second plugin under an id already loaded.
    fn reject_duplicate(&self, id: &str) -> Result<()> {
        if self.plugins.iter().any(|p| p.manifest.id == id) {
            return Err(Error::new(format!(
                "plugin `{id}` is already loaded: manifest ids must be unique"
            )));
        }
        Ok(())
    }

    /// Instantiate one WASM plugin. `capabilities` is the effective grant
    /// (already intersected by the caller); `config` are the operator's values.
    ///
    /// A component that fails to load or whose `init` returns `Err` is not
    /// added, but one bad plugin never stops the others: the error is
    /// returned for the caller to log.
    pub fn add(
        &mut self,
        file: &ManifestFile,
        capabilities: &[String],
        config: &BTreeMap<String, String>,
    ) -> Result<()> {
        let manifest = file.manifest.clone();
        if manifest.tier != PluginTier::Wasm {
            return Err(Error::new(format!(
                "plugin `{}` is `{:?}`, not a WASM component",
                manifest.id, manifest.tier
            )));
        }
        // Ids are the KV namespace and the log tag; two plugins sharing one
        // would read and rewrite each other's state.
        self.reject_duplicate(&manifest.id)?;
        self.check_requires(&manifest)?;
        let entry = manifest.entry_path(&file.dir);
        if !entry.is_file() {
            return Err(Error::new(format!(
                "plugin `{}`: component `{}` does not exist",
                manifest.id,
                entry.display()
            )));
        }
        let component = Component::from_file(&self.engine, &entry)?;
        let mut linker = Linker::<StoreData>::new(&self.engine);
        // The sandboxing context: `new()` grants no filesystem, environment or
        // network access (WASI's own defaults deny every address), so a
        // `std`-linked component can use its allocator and panic machinery
        // without gaining a way out of the sandbox.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        // One linker call covers every import a component may use: the plugin
        // host interface (`host`), the shared types, and the SDK services.
        // Using the combined world avoids defining `aether:plugin/types` twice,
        // which two separate `add_to_linker` calls would do. A component that
        // imports none of the SDK interfaces (the frozen `@0.1.0` ones
        // included) is unaffected.
        crate::sdk::Services::add_to_linker::<_, HasSelf<_>>(&mut linker, |d: &mut StoreData| d)?;
        // The menus service interface, so a WASM dependent that imports it
        // links and its calls forward to the native menus provider.
        crate::menus_svc::Services::add_to_linker::<_, HasSelf<_>>(
            &mut linker,
            |d: &mut StoreData| d,
        )?;

        let memory = self.config.memory_mib.saturating_mul(1024 * 1024);
        let limits = StoreLimitsBuilder::new()
            .memory_size(memory as usize)
            .build();
        let data = StoreData {
            plugin: manifest.id.clone(),
            capabilities: capabilities.to_vec(),
            config: config.clone(),
            backend: Arc::clone(&self.backend),
            services: Arc::clone(&self.services),
            limits,
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        };
        let mut store = Store::new(&self.engine, data);
        store.limiter(|d| &mut d.limits);
        // Init is bounded by fuel, not the epoch clock: park the epoch
        // deadline out of reach so the ticker cannot interrupt loading, and
        // grant a fixed fuel budget for instantiation and `init` instead.
        // A ticker tick is 1 ms, so this is about eleven days: far enough out
        // that no loading step is ever interrupted by the epoch clock.
        store.set_epoch_deadline(1_000_000_000);
        store.set_fuel(self.config.init_fuel)?;

        let bindings = Plugin::instantiate(&mut store, &component, &linker)?;
        let ack = bindings.call_init(&mut store, &config_json(config))?;
        // Hand subsequent (timed) event calls an effectively unlimited fuel
        // budget; the epoch deadline is what bounds them.
        store.set_fuel(u64::MAX)?;
        let ack =
            ack.map_err(|e| Error::new(format!("plugin `{}` refused init: {e}", manifest.id)))?;
        if ack.id != manifest.id {
            return Err(Error::new(format!(
                "plugin `{}` acknowledged as `{}`: the manifest and the component disagree",
                manifest.id, ack.id
            )));
        }
        // A plugin exists now, so timed event calls will happen: start the
        // epoch clock they are bounded by.
        self.ensure_ticker()?;
        self.plugins.push(LoadedPlugin {
            manifest,
            kind: Kind::Wasm {
                store,
                bindings: Box::new(bindings),
            },
            capabilities: capabilities.to_vec(),
            config: config.clone(),
            faults: 0,
            disabled: false,
        });
        self.sort();
        Ok(())
    }

    /// Load one native plugin. `plugin` is a fresh instance from the build-time
    /// registry; `capabilities`/`config` are the operator's effective values.
    ///
    /// Native plugins are trusted-by-install, so there is no sandbox and no
    /// `init` fuel: `init` may register services and load state, and a panic is
    /// the operator's problem, not something the host can contain.
    pub fn add_native(
        &mut self,
        file: &ManifestFile,
        capabilities: &[String],
        config: &BTreeMap<String, String>,
        mut plugin: Box<dyn NativePlugin>,
    ) -> Result<()> {
        let manifest = file.manifest.clone();
        if manifest.tier != PluginTier::Native {
            return Err(Error::new(format!(
                "plugin `{}` is `{:?}`, not a native plugin",
                manifest.id, manifest.tier
            )));
        }
        self.reject_duplicate(&manifest.id)?;
        self.check_requires(&manifest)?;
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        let mut ctx = NativeContext {
            id: &manifest.id,
            capabilities,
            config,
            services: &services,
            backend: &backend,
        };
        plugin.init(&mut ctx).map_err(|e| {
            Error::new(format!("native plugin `{}` refused init: {e}", manifest.id))
        })?;
        self.plugins.push(LoadedPlugin {
            manifest,
            kind: Kind::Native(plugin),
            capabilities: capabilities.to_vec(),
            config: config.clone(),
            faults: 0,
            disabled: false,
        });
        self.sort();
        Ok(())
    }

    /// Order plugins by descending priority, then by id for a total order.
    fn sort(&mut self) {
        self.plugins.sort_by(|a, b| {
            b.manifest
                .priority
                .cmp(&a.manifest.priority)
                .then_with(|| a.manifest.id.cmp(&b.manifest.id))
        });
    }

    /// The ids of the loaded, enabled plugins, in call order.
    pub fn ids(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter(|p| !p.disabled)
            .map(|p| p.manifest.id.clone())
            .collect()
    }

    /// Call `shutdown` on every plugin, best effort.
    pub fn shutdown(&mut self) {
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for p in &mut self.plugins {
            if p.disabled {
                continue;
            }
            match &mut p.kind {
                Kind::Wasm { store, bindings } => {
                    store.set_epoch_deadline(self.config.budget_ms.max(1));
                    let _ = bindings.call_shutdown(store);
                }
                Kind::Native(n) => {
                    let mut ctx = NativeContext {
                        id: &p.manifest.id,
                        capabilities: &p.capabilities,
                        config: &p.config,
                        services: &services,
                        backend: &backend,
                    };
                    n.shutdown(&mut ctx);
                }
            }
        }
    }

    /// Record a fault and disable the plugin at the limit.
    fn fault(&mut self, i: usize, event: &str, err: impl std::fmt::Display) {
        let (id, faults) = {
            let p = &mut self.plugins[i];
            p.faults += 1;
            (p.manifest.id.clone(), p.faults)
        };
        let disabled = faults >= self.config.max_faults;
        if disabled {
            self.plugins[i].disabled = true;
        }
        let p = &self.plugins[i];
        self.backend.log(
            &id,
            LogLevel::Error,
            &format!(
                "trap in `{event}` (fault {}/{}{}): {err}",
                faults,
                self.config.max_faults,
                if disabled { ", disabled" } else { "" }
            ),
        );
        let _ = p;
    }

    /// Call every subscribed plugin, in order, until one cancels. A trap is
    /// treated as `allow` — a broken plugin must not silently veto gameplay.
    fn before(
        &mut self,
        event: &str,
        mut f: impl FnMut(
            &mut LoadedPlugin,
            &Arc<ServiceRegistry>,
            &Arc<dyn HostBackend>,
        ) -> std::result::Result<Decision, String>,
    ) -> Decision {
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for i in 0..self.plugins.len() {
            if !self.plugins[i].receives(event) {
                continue;
            }
            let ms = self.call_budget_ms();
            if ms == 0 {
                // The tick's shared budget is gone: no plugin runs again this
                // tick. This is what bounds the *aggregate* cost, not just one
                // call. A fail-closed plugin cannot run, so it vetoes.
                if self.plugins[i].manifest.deny_on_fault {
                    return Decision::Cancel(format!(
                        "plugin `{}` could not check this action: the plugin budget is spent",
                        self.plugins[i].manifest.id
                    ));
                }
                continue;
            }
            self.plugins[i].set_deadline(ms);
            let started = Instant::now();
            let r = f(&mut self.plugins[i], &services, &backend);
            self.charge_tick(started, ms, r.is_err());
            match r {
                Ok(Decision::Allow) => {}
                Ok(d @ Decision::Cancel(_)) => return d,
                Err(e) => {
                    // Default: a trap is `allow` (a broken plugin must not
                    // silently veto). A fail-closed plugin flips that.
                    let fail_closed = self.plugins[i].manifest.deny_on_fault;
                    let id = self.plugins[i].manifest.id.clone();
                    self.fault(i, event, e);
                    if fail_closed {
                        return Decision::Cancel(format!("plugin `{id}` failed in `{event}`"));
                    }
                }
            }
        }
        Decision::Allow
    }

    /// Call every subscribed plugin, ignoring their result.
    fn observe(
        &mut self,
        event: &str,
        mut f: impl FnMut(
            &mut LoadedPlugin,
            &Arc<ServiceRegistry>,
            &Arc<dyn HostBackend>,
        ) -> std::result::Result<(), String>,
    ) {
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for i in 0..self.plugins.len() {
            if !self.plugins[i].receives(event) {
                continue;
            }
            let ms = self.call_budget_ms();
            if ms == 0 {
                continue;
            }
            self.plugins[i].set_deadline(ms);
            let started = Instant::now();
            let r = f(&mut self.plugins[i], &services, &backend);
            self.charge_tick(started, ms, r.is_err());
            if let Err(e) = r {
                self.fault(i, event, e);
            }
        }
    }

    // --- before-* ------------------------------------------------------

    /// `before-block-break`.
    pub fn before_block_break(
        &mut self,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Decision {
        self.before(manifest::EVENT_BEFORE_BLOCK_BREAK, |pl, s, b| {
            pl.call_before_block_break(s, b, p, x, y, z, block)
        })
    }

    /// `before-block-place`.
    pub fn before_block_place(
        &mut self,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Decision {
        self.before(manifest::EVENT_BEFORE_BLOCK_PLACE, |pl, s, b| {
            pl.call_before_block_place(s, b, p, x, y, z, block)
        })
    }

    /// `before-use-item`.
    pub fn before_use_item(&mut self, p: &Player, item: &str) -> Decision {
        self.before(manifest::EVENT_BEFORE_USE_ITEM, |pl, s, b| {
            pl.call_before_use_item(s, b, p, item)
        })
    }

    /// `before-block-interact`: a native-only right-click on a block, with the
    /// clicked cell. A WASM plugin subscribed to the name is skipped (its
    /// world has no such export), so the call is free of a boundary crossing.
    pub fn before_block_interact(
        &mut self,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Decision {
        self.before(manifest::EVENT_BEFORE_BLOCK_INTERACT, |pl, s, b| {
            pl.call_before_block_interact(s, b, p, x, y, z, block)
        })
    }

    /// `before-block-dig`: a native-only start-of-mining hook, called before
    /// the region check and before a survival dig begins. A WASM plugin is
    /// skipped, since its world has no such export.
    pub fn before_block_dig(
        &mut self,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Decision {
        self.before(manifest::EVENT_BEFORE_BLOCK_DIG, |pl, s, b| {
            pl.call_before_block_dig(s, b, p, x, y, z, block)
        })
    }

    /// `before-use-block`: a right-click on a block with an item. Native-only,
    /// used by the claims golden shovel.
    #[allow(clippy::too_many_arguments)]
    pub fn before_use_block(
        &mut self,
        p: &Player,
        x: i32,
        y: i32,
        z: i32,
        item: &str,
        block: &str,
    ) -> Decision {
        self.before(manifest::EVENT_BEFORE_USE_BLOCK, |pl, s, b| {
            pl.call_before_use_block(s, b, p, x, y, z, item, block)
        })
    }

    /// `before-attack`.
    pub fn before_attack(&mut self, p: &Player, target: u64) -> Decision {
        self.before(manifest::EVENT_BEFORE_ATTACK, |pl, s, b| {
            pl.call_before_attack(s, b, p, target)
        })
    }

    /// `before-chat`.
    pub fn before_chat(&mut self, p: &Player, message: &str) -> Decision {
        self.before(manifest::EVENT_BEFORE_CHAT, |pl, s, b| {
            pl.call_before_chat(s, b, p, message)
        })
    }

    /// `before-container-click`.
    pub fn before_container_click(&mut self, p: &Player, window: u32, slot: i32) -> Decision {
        self.before(manifest::EVENT_BEFORE_CONTAINER_CLICK, |pl, s, b| {
            pl.call_before_container_click(s, b, p, window, slot)
        })
    }

    /// A click on a virtual menu window, delivered **only to the native
    /// plugin that opened it** (`owner`).
    ///
    /// There is no manifest subscription and no WASM path: a menu click is a
    /// private channel between the core and the native plugin that owns the
    /// window (in practice the menus service host). It is not an event other
    /// plugins observe, so it cannot be cancelled and is never counted against
    /// the event budget it is not part of. The owner tag keeps every other
    /// native plugin from observing every menu's clicks.
    pub fn menu_click(
        &mut self,
        owner: &str,
        p: &Player,
        menu: u64,
        slot: i32,
        button: i8,
        mode: i32,
    ) {
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for i in 0..self.plugins.len() {
            if self.plugins[i].disabled || self.plugins[i].manifest.id != owner {
                continue;
            }
            let id = self.plugins[i].manifest.id.clone();
            let capabilities = self.plugins[i].capabilities.clone();
            let config = self.plugins[i].config.clone();
            if let Kind::Native(n) = &mut self.plugins[i].kind {
                let mut ctx = NativeContext {
                    id: &id,
                    capabilities: &capabilities,
                    config: &config,
                    services: &services,
                    backend: &backend,
                };
                n.on_menu_click(&mut ctx, p, menu, slot, button, mode);
            }
        }
    }

    /// A player closed a virtual menu window: tell the native plugin that owns
    /// it.
    pub fn menu_close(&mut self, owner: &str, p: &Player, menu: u64) {
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for i in 0..self.plugins.len() {
            if self.plugins[i].disabled || self.plugins[i].manifest.id != owner {
                continue;
            }
            let id = self.plugins[i].manifest.id.clone();
            let capabilities = self.plugins[i].capabilities.clone();
            let config = self.plugins[i].config.clone();
            if let Kind::Native(n) = &mut self.plugins[i].kind {
                let mut ctx = NativeContext {
                    id: &id,
                    capabilities: &capabilities,
                    config: &config,
                    services: &services,
                    backend: &backend,
                };
                n.on_menu_close(&mut ctx, p, menu);
            }
        }
    }

    // --- after-* -------------------------------------------------------

    /// `after-player-join`.
    pub fn after_player_join(&mut self, p: &Player) {
        self.observe(manifest::EVENT_AFTER_PLAYER_JOIN, |pl, s, b| {
            pl.call_after_player_join(s, b, p)
        });
    }

    /// `after-player-leave`.
    pub fn after_player_leave(&mut self, p: &Player) {
        self.observe(manifest::EVENT_AFTER_PLAYER_LEAVE, |pl, s, b| {
            pl.call_after_player_leave(s, b, p)
        });
    }

    /// `after-block-break`.
    pub fn after_block_break(&mut self, p: &Player, x: i32, y: i32, z: i32, block: &str) {
        self.observe(manifest::EVENT_AFTER_BLOCK_BREAK, |pl, s, b| {
            pl.call_after_block_break(s, b, p, x, y, z, block)
        });
    }

    /// `after-block-place`. Native-only (see
    /// [`LoadedPlugin::call_after_block_place`]).
    pub fn after_block_place(&mut self, p: &Player, x: i32, y: i32, z: i32, block: &str) {
        self.observe(manifest::EVENT_AFTER_BLOCK_PLACE, |pl, s, b| {
            pl.call_after_block_place(s, b, p, x, y, z, block)
        });
    }

    /// `after-entity-death`.
    pub fn after_entity_death(&mut self, entity: u64, killer: Option<u64>) {
        self.observe(manifest::EVENT_AFTER_ENTITY_DEATH, |pl, s, b| {
            pl.call_after_entity_death(s, b, entity, killer)
        });
    }

    /// `after-damage`.
    pub fn after_damage(&mut self, entity: u64, amount: f64, source: &str) {
        self.observe(manifest::EVENT_AFTER_DAMAGE, |pl, s, b| {
            pl.call_after_damage(s, b, entity, amount, source)
        });
    }

    /// `after-craft`.
    pub fn after_craft(&mut self, p: &Player, item: &str, count: u32) {
        self.observe(manifest::EVENT_AFTER_CRAFT, |pl, s, b| {
            pl.call_after_craft(s, b, p, item, count)
        });
    }

    /// `after-enchant`.
    pub fn after_enchant(&mut self, p: &Player, item: &str, level: u32) {
        self.observe(manifest::EVENT_AFTER_ENCHANT, |pl, s, b| {
            pl.call_after_enchant(s, b, p, item, level)
        });
    }

    // --- tick and commands ---------------------------------------------

    /// `on-tick`: reset the shared tick budget, then call subscribed plugins
    /// when `n` is on their period.
    pub fn on_tick(&mut self, n: u64) {
        self.begin_tick();
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for i in 0..self.plugins.len() {
            if !self.plugins[i].receives(manifest::EVENT_ON_TICK) {
                continue;
            }
            let period = self.plugins[i].manifest.tick_period.max(1);
            if n % period != 0 {
                continue;
            }
            let ms = self.call_budget_ms();
            if ms == 0 {
                continue;
            }
            self.plugins[i].set_deadline(ms);
            let started = Instant::now();
            let r = self.plugins[i].call_on_tick(&services, &backend, n);
            self.charge_tick(started, ms, r.is_err());
            if let Err(e) = r {
                self.fault(i, manifest::EVENT_ON_TICK, e);
            }
        }
    }

    /// `on-command`: the first plugin that handles the command answers it.
    ///
    /// A reserved built-in name never reaches a plugin, so a plugin cannot
    /// shadow the built-in command and its permission check.
    pub fn on_command(&mut self, p: &Player, name: &str, args: &[String]) -> Option<CommandResult> {
        if self.config.reserved_commands.iter().any(|c| c == name) {
            self.backend.log(
                "host",
                LogLevel::Warn,
                &format!("refused to route built-in command `{name}` to a plugin"),
            );
            return None;
        }
        let event = manifest::EVENT_ON_COMMAND;
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for i in 0..self.plugins.len() {
            if !self.plugins[i].receives(event) || !self.plugins[i].manifest.serves_command(name) {
                continue;
            }
            let ms = self.call_budget_ms();
            if ms == 0 {
                continue;
            }
            self.plugins[i].set_deadline(ms);
            let started = Instant::now();
            let r = self.plugins[i].call_on_command(&services, &backend, p, name, args);
            self.charge_tick(started, ms, r.is_err());
            match r {
                Ok(result) if result.handled => return Some(result),
                Ok(_) => continue,
                Err(e) => self.fault(i, event, e),
            }
        }
        None
    }

    /// `on-plugin-message`: every native plugin subscribed to the event is
    /// notified. It is observe-only — a plugin cannot cancel or rewrite the
    /// payload.
    pub fn on_plugin_message(&mut self, p: &Player, channel: &str, data: &[u8]) {
        let event = manifest::EVENT_ON_PLUGIN_MESSAGE;
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for i in 0..self.plugins.len() {
            if !self.plugins[i].receives(event) {
                continue;
            }
            let ms = self.call_budget_ms();
            if ms == 0 {
                continue;
            }
            self.plugins[i].set_deadline(ms);
            let started = Instant::now();
            let r = self.plugins[i].call_on_plugin_message(&services, &backend, p, channel, data);
            self.charge_tick(started, ms, r.is_err());
            if let Err(e) = r {
                self.fault(i, event, e);
            }
        }
    }

    /// `on-command` for a **specific** plugin, resolved by the caller from the
    /// core command registry rather than the manifest's static command list.
    ///
    /// A plugin that registers commands at runtime (the menus host's
    /// declarative `open_commands`) cannot list them in a static manifest, so
    /// the core registry — the source of truth for who owns a name — tells the
    /// server which plugin to ask. Ownership is already decided there, so this
    /// does not re-check `serves_command`; it only checks the plugin exists and
    /// is subscribed and enabled.
    pub fn on_command_for(
        &mut self,
        plugin: &str,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<CommandResult> {
        let event = manifest::EVENT_ON_COMMAND;
        let services = Arc::clone(&self.services);
        let backend = Arc::clone(&self.backend);
        for i in 0..self.plugins.len() {
            if self.plugins[i].manifest.id != plugin || !self.plugins[i].receives(event) {
                continue;
            }
            let ms = self.call_budget_ms();
            if ms == 0 {
                return None;
            }
            self.plugins[i].set_deadline(ms);
            let started = Instant::now();
            let r = self.plugins[i].call_on_command(&services, &backend, p, name, args);
            self.charge_tick(started, ms, r.is_err());
            return match r {
                Ok(result) if result.handled => Some(result),
                Ok(_) => None,
                Err(e) => {
                    self.fault(i, event, e);
                    None
                }
            };
        }
        None
    }
}

/// The config handed to `init` as JSON. Written by hand: it is flat and made
/// only of strings.
fn config_json(config: &BTreeMap<String, String>) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in config.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_string(k));
        out.push(':');
        out.push_str(&json_string(v));
    }
    out.push('}');
    out
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Manifest, PluginTier};
    use crate::native::{NativeContext, NativePlugin, NativeRegistration};
    use std::sync::Mutex;

    /// A backend with no world: the menu-click routing test needs none.
    #[derive(Default)]
    struct Null;

    impl HostBackend for Null {
        fn kv_get(&self, _: &str, _: &str) -> Option<Vec<u8>> {
            None
        }
        fn kv_put(&self, _: &str, _: &str, _: &[u8]) {}
        fn kv_delete(&self, _: &str, _: &str) {}
        fn kv_list(&self, _: &str, _: &str) -> Vec<String> {
            Vec::new()
        }
        fn log(&self, _: &str, _: LogLevel, _: &str) {}
        fn config(&self, _: &str, _: &str) -> Option<String> {
            None
        }
        fn supports(&self, _: &str, _: &str) -> bool {
            false
        }
    }

    /// Records which plugin id received a menu click.
    static CLICKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct Clicker;

    impl NativePlugin for Clicker {
        fn init(&mut self, _ctx: &mut NativeContext) -> std::result::Result<(), String> {
            Ok(())
        }
        fn on_menu_click(
            &mut self,
            ctx: &mut NativeContext,
            _p: &Player,
            _menu: u64,
            _slot: i32,
            _button: i8,
            _mode: i32,
        ) {
            CLICKS.lock().unwrap().push(ctx.id.to_string());
        }
    }

    fn native(id: &str) -> NativeRegistration {
        NativeRegistration {
            manifest: Manifest {
                id: id.to_string(),
                version: "0.0.0".to_string(),
                api: "0.1".to_string(),
                entry: String::new(),
                name: String::new(),
                priority: 0,
                capabilities: vec![],
                subscribe: vec![],
                commands: vec![],
                requires: Vec::new(),
                tick_period: 1,
                deny_on_fault: false,
                tier: PluginTier::Native,
            },
            new: || Box::new(Clicker),
        }
    }

    #[test]
    fn a_menu_click_reaches_only_the_owning_native_plugin() {
        let mut host = PluginHost::new(HostConfig::default(), Arc::new(Null)).unwrap();
        for id in ["alpha", "beta"] {
            let reg = native(id);
            let file = ManifestFile {
                dir: std::path::PathBuf::new(),
                manifest: reg.manifest,
            };
            host.add_native(&file, &[], &BTreeMap::new(), (reg.new)())
                .unwrap();
        }
        CLICKS.lock().unwrap().clear();
        let p = Player {
            id: 1,
            name: "Steve".to_string(),
            operator: false,
        };
        host.menu_click("alpha", &p, 7, 0, 0, 0);
        assert_eq!(
            CLICKS.lock().unwrap().as_slice(),
            ["alpha"],
            "only the owner sees its menus' clicks"
        );
    }
}
