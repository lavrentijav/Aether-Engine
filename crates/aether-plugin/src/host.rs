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
use crate::manifest::{self, ManifestFile};
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
}

/// Sandbox and scheduling limits for the whole host.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Per-call time budget, in milliseconds. Enforced with epoch interruption.
    pub budget_ms: u64,
    /// Shared time budget, in milliseconds, spent by **all** plugins across
    /// one server tick. Once it is gone no further plugin is called until the
    /// next tick, so a plugin (or many) cannot stall the tick even when each
    /// individual call stays inside `budget_ms`.
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
struct StoreData {
    plugin: String,
    capabilities: Vec<String>,
    config: BTreeMap<String, String>,
    backend: Arc<dyn HostBackend>,
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
    fn allowed(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// Refuse an ungranted call: log the denial and answer with the neutral
    /// value. Panicking would let a manifest typo take the server down, which
    /// is exactly what a sandbox is for.
    fn deny(&self, capability: &str, what: &str) {
        self.backend.log(
            &self.plugin,
            LogLevel::Warn,
            &format!("capability `{capability}` not granted; refused {what}"),
        );
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

/// One instantiated plugin.
struct LoadedPlugin {
    manifest: manifest::Manifest,
    store: Store<StoreData>,
    bindings: Plugin,
    faults: u32,
    disabled: bool,
}

impl LoadedPlugin {
    /// Whether this plugin should receive `event` right now.
    fn receives(&self, event: &str) -> bool {
        !self.disabled && self.manifest.subscribes(event)
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
    plugins: Vec<LoadedPlugin>,
    /// When the current tick's shared budget runs out. `None` until the first
    /// [`on_tick`]; before any tick there is no tick to overrun, so calls are
    /// bounded only by the per-call budget. Reset by every [`on_tick`].
    tick_deadline: Option<Instant>,
    /// The shared budget as a duration, kept beside the deadline.
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
            plugins: Vec::new(),
            tick_deadline: None,
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

    /// Reset the shared tick budget. Called at the start of every server tick,
    /// before any plugin for that tick runs.
    fn begin_tick(&mut self) {
        self.tick_deadline = Some(Instant::now() + self.tick_budget);
    }

    /// The epoch deadline, in milliseconds, for the next plugin call: the
    /// per-call budget, further capped by what is left of the shared tick
    /// budget. **Zero means the tick's budget is spent** and no further plugin
    /// should run this tick.
    fn call_budget_ms(&self) -> u64 {
        let per_call = self.config.budget_ms.max(1);
        match self.tick_deadline {
            // No tick has run yet: there is no window to spend.
            None => per_call,
            Some(deadline) => {
                let remaining = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64;
                per_call.min(remaining)
            }
        }
    }

    /// Instantiate one plugin. `capabilities` is the effective grant (already
    /// intersected by the caller); `config` are the operator's values.
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
        // Ids are the KV namespace and the log tag; two plugins sharing one
        // would read and rewrite each other's state.
        if self.plugins.iter().any(|p| p.manifest.id == manifest.id) {
            return Err(Error::new(format!(
                "plugin `{}` is already loaded: manifest ids must be unique",
                manifest.id
            )));
        }
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
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |d: &mut StoreData| d)?;

        let memory = self.config.memory_mib.saturating_mul(1024 * 1024);
        let limits = StoreLimitsBuilder::new()
            .memory_size(memory as usize)
            .build();
        let data = StoreData {
            plugin: manifest.id.clone(),
            capabilities: capabilities.to_vec(),
            config: config.clone(),
            backend: Arc::clone(&self.backend),
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
            store,
            bindings,
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
        for p in &mut self.plugins {
            if p.disabled {
                continue;
            }
            p.store.set_epoch_deadline(self.config.budget_ms.max(1));
            let _ = p.bindings.call_shutdown(&mut p.store);
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
        mut f: impl FnMut(&mut LoadedPlugin) -> wasmtime::Result<Decision>,
    ) -> Decision {
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
            self.plugins[i].store.set_epoch_deadline(ms);
            let r = {
                let pl = &mut self.plugins[i];
                f(pl)
            };
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
        mut f: impl FnMut(&mut LoadedPlugin) -> wasmtime::Result<()>,
    ) {
        for i in 0..self.plugins.len() {
            if !self.plugins[i].receives(event) {
                continue;
            }
            let ms = self.call_budget_ms();
            if ms == 0 {
                continue;
            }
            self.plugins[i].store.set_epoch_deadline(ms);
            let r = {
                let pl = &mut self.plugins[i];
                f(pl)
            };
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
        self.before(manifest::EVENT_BEFORE_BLOCK_BREAK, |pl| {
            pl.bindings
                .call_before_block_break(&mut pl.store, p, x, y, z, block)
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
        self.before(manifest::EVENT_BEFORE_BLOCK_PLACE, |pl| {
            pl.bindings
                .call_before_block_place(&mut pl.store, p, x, y, z, block)
        })
    }

    /// `before-use-item`.
    pub fn before_use_item(&mut self, p: &Player, item: &str) -> Decision {
        self.before(manifest::EVENT_BEFORE_USE_ITEM, |pl| {
            pl.bindings.call_before_use_item(&mut pl.store, p, item)
        })
    }

    /// `before-attack`.
    pub fn before_attack(&mut self, p: &Player, target: u64) -> Decision {
        self.before(manifest::EVENT_BEFORE_ATTACK, |pl| {
            pl.bindings.call_before_attack(&mut pl.store, p, target)
        })
    }

    /// `before-chat`.
    pub fn before_chat(&mut self, p: &Player, message: &str) -> Decision {
        self.before(manifest::EVENT_BEFORE_CHAT, |pl| {
            pl.bindings.call_before_chat(&mut pl.store, p, message)
        })
    }

    /// `before-container-click`.
    pub fn before_container_click(&mut self, p: &Player, window: u32, slot: i32) -> Decision {
        self.before(manifest::EVENT_BEFORE_CONTAINER_CLICK, |pl| {
            pl.bindings
                .call_before_container_click(&mut pl.store, p, window, slot)
        })
    }

    // --- after-* -------------------------------------------------------

    /// `after-player-join`.
    pub fn after_player_join(&mut self, p: &Player) {
        self.observe(manifest::EVENT_AFTER_PLAYER_JOIN, |pl| {
            pl.bindings.call_after_player_join(&mut pl.store, p)
        });
    }

    /// `after-player-leave`.
    pub fn after_player_leave(&mut self, p: &Player) {
        self.observe(manifest::EVENT_AFTER_PLAYER_LEAVE, |pl| {
            pl.bindings.call_after_player_leave(&mut pl.store, p)
        });
    }

    /// `after-block-break`.
    pub fn after_block_break(&mut self, p: &Player, x: i32, y: i32, z: i32, block: &str) {
        self.observe(manifest::EVENT_AFTER_BLOCK_BREAK, |pl| {
            pl.bindings
                .call_after_block_break(&mut pl.store, p, x, y, z, block)
        });
    }

    /// `after-entity-death`.
    pub fn after_entity_death(&mut self, entity: u64, killer: Option<u64>) {
        self.observe(manifest::EVENT_AFTER_ENTITY_DEATH, |pl| {
            pl.bindings
                .call_after_entity_death(&mut pl.store, entity, killer)
        });
    }

    /// `after-damage`.
    pub fn after_damage(&mut self, entity: u64, amount: f64, source: &str) {
        self.observe(manifest::EVENT_AFTER_DAMAGE, |pl| {
            pl.bindings
                .call_after_damage(&mut pl.store, entity, amount, source)
        });
    }

    /// `after-craft`.
    pub fn after_craft(&mut self, p: &Player, item: &str, count: u32) {
        self.observe(manifest::EVENT_AFTER_CRAFT, |pl| {
            pl.bindings.call_after_craft(&mut pl.store, p, item, count)
        });
    }

    /// `after-enchant`.
    pub fn after_enchant(&mut self, p: &Player, item: &str, level: u32) {
        self.observe(manifest::EVENT_AFTER_ENCHANT, |pl| {
            pl.bindings
                .call_after_enchant(&mut pl.store, p, item, level)
        });
    }

    // --- tick and commands ---------------------------------------------

    /// `on-tick`: reset the shared tick budget, then call subscribed plugins
    /// when `n` is on their period.
    pub fn on_tick(&mut self, n: u64) {
        self.begin_tick();
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
            self.plugins[i].store.set_epoch_deadline(ms);
            let r = {
                let pl = &mut self.plugins[i];
                pl.bindings.call_on_tick(&mut pl.store, n)
            };
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
        for i in 0..self.plugins.len() {
            if !self.plugins[i].receives(event) || !self.plugins[i].manifest.serves_command(name) {
                continue;
            }
            let ms = self.call_budget_ms();
            if ms == 0 {
                continue;
            }
            self.plugins[i].store.set_epoch_deadline(ms);
            let r = {
                let pl = &mut self.plugins[i];
                pl.bindings.call_on_command(&mut pl.store, p, name, args)
            };
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
