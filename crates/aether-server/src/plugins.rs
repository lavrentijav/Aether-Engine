//! Where the server calls the plugin host.
//!
//! Every hook point in the game calls one of these small functions. With the
//! `plugins` feature off they compile away to the neutral answer — `Ok(())`,
//! `None`, nothing — so the game code has one shape whether or not Wasmtime is
//! in the build. With it on, the first call installs a process-wide
//! [`PluginHost`] and later calls take its lock briefly.
//!
//! The event functions never unwind into the game: a plugin that traps is
//! counted and, at the limit, disabled by the host; the caller only ever sees
//! `Allow` or a cancel reason.

#[cfg(feature = "plugins")]
mod imp {
    use std::path::Path;
    use std::sync::{Arc, Mutex, OnceLock};

    use aether_plugin::{
        discover, Decision, HostBackend, HostConfig, LogLevel, Player, PluginHost,
    };
    use aether_world::KvBackend;

    use crate::config::Config;
    use crate::players::PlayerHandle;
    use crate::session::DemoWorld;

    /// Features `supports()` reports. Versioned names, as the design asks, so
    /// a plugin probes rather than assumes.
    const SUPPORTED: &[&str] = &[
        "plugin.v1",
        "event-bus.v1",
        "storage.kv.v1",
        "log.v1",
        "config.v1",
    ];

    /// KV keys live under this prefix, which no other part of the world store
    /// uses: sub-chunks are `ASC2…`, registries `REG`/`ITM`, player records
    /// `P…`.
    const KV_PREFIX: &[u8] = b"aether-plugin/";

    fn kv_key(plugin: &str, key: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(KV_PREFIX.len() + plugin.len() + key.len() + 2);
        out.extend_from_slice(KV_PREFIX);
        out.extend_from_slice(plugin.as_bytes());
        out.push(b'/');
        out.extend_from_slice(key.as_bytes());
        out
    }

    fn kv_prefix(plugin: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(KV_PREFIX.len() + plugin.len() + 1);
        out.extend_from_slice(KV_PREFIX);
        out.extend_from_slice(plugin.as_bytes());
        out.push(b'/');
        out
    }

    /// The world and operator config behind the host imports.
    struct ServerBackend {
        world: Arc<DemoWorld>,
        config: Arc<Config>,
    }

    impl HostBackend for ServerBackend {
        fn kv_get(&self, plugin: &str, key: &str) -> Option<Vec<u8>> {
            self.world
                .journal()
                .backend()
                .get(&kv_key(plugin, key))
                .ok()
                .flatten()
        }

        fn kv_put(&self, plugin: &str, key: &str, value: &[u8]) {
            if let Err(e) = self
                .world
                .journal()
                .backend()
                .put(&kv_key(plugin, key), value)
            {
                crate::log::warn(&format!("plugin `{plugin}`: kv-put failed: {e}"));
            }
        }

        fn kv_delete(&self, plugin: &str, key: &str) {
            if let Err(e) = self.world.journal().backend().delete(&kv_key(plugin, key)) {
                crate::log::warn(&format!("plugin `{plugin}`: kv-delete failed: {e}"));
            }
        }

        fn kv_list(&self, plugin: &str, prefix: &str) -> Vec<String> {
            let full = kv_key(plugin, prefix);
            let base = kv_prefix(plugin).len();
            match self.world.journal().backend().scan_prefix(&full) {
                Ok(rows) => rows
                    .into_iter()
                    .filter_map(|(k, _)| String::from_utf8(k[base..].to_vec()).ok())
                    .collect(),
                Err(e) => {
                    crate::log::warn(&format!("plugin `{plugin}`: kv-list failed: {e}"));
                    Vec::new()
                }
            }
        }

        fn log(&self, plugin: &str, level: LogLevel, message: &str) {
            let line = format!("[plugin:{plugin}] {message}");
            match level {
                LogLevel::Error => crate::log::error(&line),
                LogLevel::Warn => crate::log::warn(&line),
                LogLevel::Trace | LogLevel::Debug | LogLevel::Info => crate::log::info(&line),
            }
        }

        fn config(&self, plugin: &str, key: &str) -> Option<String> {
            self.config
                .plugins
                .plugins
                .get(plugin)
                .and_then(|e| e.config.get(key))
                .cloned()
        }

        fn supports(&self, _plugin: &str, feature: &str) -> bool {
            SUPPORTED.contains(&feature)
        }
    }

    static HOST: OnceLock<Mutex<PluginHost>> = OnceLock::new();

    /// Load every plugin the operator configured. Called once at startup.
    pub fn install(cfg: &Arc<Config>, world: &Arc<DemoWorld>) {
        if cfg.plugins.dir.trim().is_empty() {
            return;
        }
        let backend = Arc::new(ServerBackend {
            world: Arc::clone(world),
            config: Arc::clone(cfg),
        });
        let host_config = HostConfig {
            budget_ms: cfg.plugins.budget_ms.max(1),
            tick_budget_ms: cfg.plugins.tick_budget_ms.max(1),
            memory_mib: cfg.plugins.memory_mib.max(1),
            max_faults: cfg.plugins.max_faults.max(1),
            ..HostConfig::default()
        };
        let mut host = match PluginHost::new(host_config, backend) {
            Ok(h) => h,
            Err(e) => {
                crate::log::warn(&format!("plugins disabled: {e}"));
                return;
            }
        };
        for file in discover(Path::new(&cfg.plugins.dir)) {
            let id = file.manifest.id.clone();
            let entry = cfg.plugins.plugins.get(&id).cloned().unwrap_or_default();
            if !entry.enabled() {
                crate::log::info(&format!("plugin `{id}` disabled by config"));
                continue;
            }
            let caps = effective_capabilities(&file.manifest.capabilities, &entry.capabilities);
            match host.add(&file, &caps, &entry.config) {
                Ok(()) => crate::log::info(&format!(
                    "plugin     : {} v{} (priority {}, {} event(s))",
                    id,
                    file.manifest.version,
                    file.manifest.priority,
                    file.manifest.subscribe.len()
                )),
                Err(e) => crate::log::warn(&format!("plugin `{id}` not loaded: {e}")),
            }
        }
        let ids = host.ids();
        if ids.is_empty() {
            crate::log::info("plugin     : none active");
        }
        let _ = HOST.set(Mutex::new(host));
    }

    /// The requested capabilities, narrowed by what the operator granted. An
    /// empty operator list means "grant what the plugin asked for", which is
    /// the convenient default for a known plugin.
    fn effective_capabilities(requested: &[String], granted: &[String]) -> Vec<String> {
        if granted.is_empty() {
            return requested.to_vec();
        }
        requested
            .iter()
            .filter(|c| granted.iter().any(|g| g == *c))
            .cloned()
            .collect()
    }

    /// Call `f` with the host, if one is installed.
    fn with<R>(default: R, f: impl FnOnce(&mut PluginHost) -> R) -> R {
        match HOST.get() {
            Some(lock) => f(&mut lock.lock().unwrap()),
            None => default,
        }
    }

    fn player(handle: &PlayerHandle) -> Player {
        Player {
            id: handle.entity_id as u64,
            name: handle.name.clone(),
        }
    }

    /// The event-line the host should show for a cancelled `before-*`.
    fn cancelled(decision: Decision) -> Option<String> {
        match decision {
            Decision::Allow => None,
            Decision::Cancel(reason) => Some(reason),
        }
    }

    pub fn on_tick(n: u64) {
        with((), |h| h.on_tick(n));
    }

    pub fn before_block_break(
        handle: &PlayerHandle,
        _x: i32,
        _y: i32,
        _z: i32,
        _block: &str,
    ) -> Option<String> {
        with(None, |h| {
            cancelled(h.before_block_break(&player(handle), _x, _y, _z, _block))
        })
    }

    pub fn after_block_break(handle: &PlayerHandle, x: i32, y: i32, z: i32, block: &str) {
        with((), |h| h.after_block_break(&player(handle), x, y, z, block));
    }

    pub fn before_block_place(
        handle: &PlayerHandle,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Option<String> {
        with(None, |h| {
            cancelled(h.before_block_place(&player(handle), x, y, z, block))
        })
    }

    pub fn before_use_item(handle: &PlayerHandle, item: &str) -> Option<String> {
        with(None, |h| {
            cancelled(h.before_use_item(&player(handle), item))
        })
    }

    pub fn before_attack(handle: &PlayerHandle, target: u64) -> Option<String> {
        with(None, |h| {
            cancelled(h.before_attack(&player(handle), target))
        })
    }

    pub fn before_chat(handle: &PlayerHandle, message: &str) -> Option<String> {
        with(None, |h| cancelled(h.before_chat(&player(handle), message)))
    }

    pub fn before_container_click(handle: &PlayerHandle, window: u32, slot: i32) -> Option<String> {
        with(None, |h| {
            cancelled(h.before_container_click(&player(handle), window, slot))
        })
    }

    pub fn after_player_join(handle: &PlayerHandle) {
        with((), |h| h.after_player_join(&player(handle)));
    }

    pub fn after_player_leave(handle: &PlayerHandle) {
        with((), |h| h.after_player_leave(&player(handle)));
    }

    pub fn after_entity_death(entity: u64, killer: Option<u64>) {
        with((), |h| h.after_entity_death(entity, killer));
    }

    pub fn after_damage(entity: u64, amount: f64, source: &str) {
        with((), |h| h.after_damage(entity, amount, source));
    }

    pub fn after_craft(handle: &PlayerHandle, item: &str, count: u32) {
        with((), |h| h.after_craft(&player(handle), item, count));
    }

    // No server hook point yet: enchanting lands with the E1/E2 work, which
    // will call this. The event exists in the bus and the WIT ahead of it.
    #[allow(dead_code)]
    pub fn after_enchant(handle: &PlayerHandle, item: &str, level: u32) {
        with((), |h| h.after_enchant(&player(handle), item, level));
    }

    /// A plugin command, if one is registered. `None` lets the built-ins run.
    pub fn on_command(handle: &PlayerHandle, name: &str, args: &[String]) -> Option<Vec<String>> {
        with(None, |h| {
            h.on_command(&player(handle), name, args).map(|r| r.lines)
        })
    }

    pub fn shutdown() {
        with((), |h| h.shutdown());
    }
}

#[cfg(not(feature = "plugins"))]
mod imp {
    use crate::players::PlayerHandle;

    /// No-op: the server has no plugin host compiled in.
    pub fn install(
        _cfg: &std::sync::Arc<crate::config::Config>,
        _world: &std::sync::Arc<crate::session::DemoWorld>,
    ) {
    }
    pub fn on_tick(_n: u64) {}
    pub fn before_block_break(
        _h: &PlayerHandle,
        _x: i32,
        _y: i32,
        _z: i32,
        _b: &str,
    ) -> Option<String> {
        None
    }
    pub fn after_block_break(_h: &PlayerHandle, _x: i32, _y: i32, _z: i32, _b: &str) {}
    pub fn before_block_place(
        _h: &PlayerHandle,
        _x: i32,
        _y: i32,
        _z: i32,
        _b: &str,
    ) -> Option<String> {
        None
    }
    pub fn before_use_item(_h: &PlayerHandle, _i: &str) -> Option<String> {
        None
    }
    pub fn before_attack(_h: &PlayerHandle, _t: u64) -> Option<String> {
        None
    }
    pub fn before_chat(_h: &PlayerHandle, _m: &str) -> Option<String> {
        None
    }
    pub fn before_container_click(_h: &PlayerHandle, _w: u32, _s: i32) -> Option<String> {
        None
    }
    pub fn after_player_join(_h: &PlayerHandle) {}
    pub fn after_player_leave(_h: &PlayerHandle) {}
    pub fn after_entity_death(_e: u64, _k: Option<u64>) {}
    pub fn after_damage(_e: u64, _a: f64, _s: &str) {}
    pub fn after_craft(_h: &PlayerHandle, _i: &str, _c: u32) {}
    // See the `plugins` variant: unwired until E1/E2.
    #[allow(dead_code)]
    pub fn after_enchant(_h: &PlayerHandle, _i: &str, _l: u32) {}
    pub fn on_command(_h: &PlayerHandle, _n: &str, _a: &[String]) -> Option<Vec<String>> {
        None
    }
    pub fn shutdown() {}
}

pub use imp::*;
