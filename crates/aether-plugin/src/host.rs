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

    /// Replace a player's sidebar. The core diffs the board against what the
    /// player already shows and writes only the packets that changed, so a
    /// caller may hand over the whole board every update. The default does
    /// nothing, so a host (or a test) without a scoreboard still satisfies the
    /// trait.
    fn set_sidebar(&self, _plugin: &str, _player: u64, _board: &crate::sidebar::SidebarBoard) {}

    /// Hide a player's sidebar. The default does nothing.
    fn clear_sidebar(&self, _plugin: &str, _player: u64) {}

    /// The extra per-player facts a sidebar can show (position, world, ping,
    /// health, level). The default is `None`: a host that cannot answer simply
    /// leaves those placeholders empty.
    fn player_extra(&self, _plugin: &str, _player: u64) -> Option<crate::sidebar::PlayerExtra> {
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
