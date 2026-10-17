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

    use aether_physics::BlockView;
    use aether_plugin::sdk::{HistoryEvent, HistoryPage, HistoryPos, HistoryWindow, TeleportCause};
    use aether_plugin::{
        discover, Decision, HostBackend, HostConfig, LogLevel, ManifestFile, Player, PluginHost,
        WorldBlock,
    };
    use aether_world::journal::{self, ActorId, Event, EventBody, Place};
    use aether_world::KvBackend;

    use crate::config::Config;
    use crate::players::{PlayerHandle, SharedRegistry};
    use crate::protocol::ServerEvent;
    use crate::session::DemoWorld;

    /// Features `supports()` reports. Versioned names, as the design asks, so
    /// a plugin probes rather than assumes.
    const SUPPORTED: &[&str] = &[
        "plugin.v1",
        "event-bus.v1",
        "storage.kv.v1",
        "log.v1",
        "config.v1",
        "history.v1",
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

    /// A command a plugin asked to run from inside a host callback, held for
    /// execution **after** the host lock is released.
    ///
    /// Running it inline would re-enter the one `HOST` mutex: a menu's
    /// `[command]` reaches `commands::dispatch`, which for a non-core name
    /// calls back into `plugins::on_command`, which locks `HOST` again on the
    /// same thread — a self-deadlock. So the backend only records the command;
    /// `menu_click` drains and dispatches it once the guard has dropped.
    struct DeferredCommand {
        plugin: String,
        /// `Some` for a player command, `None` for `[console]`.
        player: Option<u64>,
        command: String,
    }

    /// The world, operator config and player registry behind the host imports.
    struct ServerBackend {
        world: Arc<DemoWorld>,
        config: Arc<Config>,
        registry: SharedRegistry,
        /// Effective capabilities per loaded plugin, filled at load. The SDK's
        /// service layer reads these to gate its privileged helpers once,
        /// keyed by caller id, so the gate applies to native and WASM
        /// dependents alike.
        granted: Mutex<std::collections::BTreeMap<String, Vec<String>>>,
        /// Commands requested from inside a host callback (see
        /// [`DeferredCommand`]), drained outside the lock.
        pending: Mutex<Vec<DeferredCommand>>,
    }

    impl ServerBackend {
        /// The plugin-visible view of an online player.
        fn player(&self, handle: &PlayerHandle) -> Player {
            Player {
                id: handle.entity_id as u64,
                name: handle.name.clone(),
                operator: crate::is_operator(handle),
            }
        }
    }

    impl ServerBackend {
        /// The online player with `id`, for the SDK `permissions` service.
        fn by_session(&self, id: u64) -> Option<Arc<PlayerHandle>> {
            self.registry.by_entity(id as i32)
        }
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

        fn kv_try_get(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
            self.world
                .journal()
                .backend()
                .get(&kv_key(plugin, key))
                .map_err(|e| format!("kv-get failed: {e}"))
        }

        fn kv_try_put(&self, plugin: &str, key: &str, value: &[u8]) -> Result<(), String> {
            self.world
                .journal()
                .backend()
                .put(&kv_key(plugin, key), value)
                .map_err(|e| format!("kv-put failed: {e}"))
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

        fn send_message(&self, _plugin: &str, player: u64, text: &str) {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return;
            };
            handle.emit(&ServerEvent::Chat(text.to_string()), &*self.world);
        }

        fn broadcast(&self, _plugin: &str, text: &str) {
            self.registry
                .broadcast(&ServerEvent::Chat(text.to_string()), &*self.world);
        }

        fn send_plugin_message(&self, _plugin: &str, player: u64, channel: &str, data: &[u8]) {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return;
            };
            handle.emit(
                &ServerEvent::PluginMessage {
                    channel: channel.to_string(),
                    data: data.to_vec(),
                },
                &*self.world,
            );
        }

        fn player_uuid(&self, _plugin: &str, player: u64) -> Option<u128> {
            self.registry.by_entity(player as i32).map(|h| h.uuid)
        }

        fn player_position(&self, _plugin: &str, player: u64) -> Option<(String, f64, f64, f64)> {
            let handle = self.registry.by_entity(player as i32)?;
            let pos = handle.pos();
            Some(("minecraft:overworld".to_string(), pos.x, pos.y, pos.z))
        }

        fn player_look(&self, _plugin: &str, player: u64) -> Option<(f32, f32)> {
            let handle = self.registry.by_entity(player as i32)?;
            let pos = handle.pos();
            Some((pos.yaw, pos.pitch))
        }

        /// `world.read`: one block for a plugin's vertical helpers. The
        /// capability is required, exactly like `players.teleport`; an
        /// ungranted caller sees "unknown" and its command refuses rather
        /// than guessing.
        fn world_block(&self, plugin: &str, x: i32, y: i32, z: i32) -> Option<WorldBlock> {
            if !self.capabilities(plugin).iter().any(|c| c == "world.read") {
                return None;
            }
            let id = self.world.get_block(x, y, z);
            let name = self.world.block_name_of(id)?;
            // Classify by the **base** block, not the state name: a
            // `minecraft:lava[level=0]` state does not end in `lava` after the
            // namespace is stripped, so only the block table names the hazard.
            let hazard = aether_world::registry::blocks::block_of_state(id)
                .is_some_and(|(_, base)| crate::teleport::is_hazard_name(base));
            Some(WorldBlock {
                solid: BlockView::is_solid(&*self.world, x, y, z),
                hazard,
                name,
            })
        }

        fn world_height(&self, plugin: &str) -> Option<(i32, i32)> {
            if !self.capabilities(plugin).iter().any(|c| c == "world.read") {
                return None;
            }
            let config = crate::teleport::SafeSpotConfig::default();
            Some((config.min_y, config.max_y))
        }

        /// CMD-31's nearest-safe-spot search. The plugin only chooses the
        /// column, radius and surface preference; the collision, hazard and
        /// height rules stay in the core.
        fn find_safe_spot(
            &self,
            plugin: &str,
            around: (i32, i32, i32),
            radius: u32,
            prefer_surface: bool,
        ) -> Option<(i32, i32, i32)> {
            if !self.capabilities(plugin).iter().any(|c| c == "world.read") {
                return None;
            }
            let config = crate::teleport::SafeSpotConfig {
                prefer_surface,
                ..crate::teleport::SafeSpotConfig::default()
            };
            crate::teleport::find_safe_spot(
                &*self.world,
                crate::teleport::BlockPos::new(around.0, around.1, around.2),
                radius,
                config,
            )
            .map(|p| (p.x, p.y, p.z))
        }

        /// CMD-31's random safe-spot search, with the core's bounded attempt
        /// budget. A miss returns `None` and the plugin reports it without
        /// moving the player.
        fn random_safe_spot(
            &self,
            plugin: &str,
            center: (i32, i32, i32),
            min_radius: u32,
            max_radius: u32,
        ) -> Option<(i32, i32, i32)> {
            if !self.capabilities(plugin).iter().any(|c| c == "world.read") {
                return None;
            }
            let mut rng = crate::game::tables::Rng::from_time();
            crate::teleport::random_safe_spot(
                &*self.world,
                crate::teleport::BlockPos::new(center.0, center.1, center.2),
                min_radius,
                max_radius,
                crate::teleport::SafeSpotConfig::default(),
                &mut rng,
            )
            .map(|p| (p.x, p.y, p.z))
        }

        fn held_item(&self, _plugin: &str, player: u64) -> Option<String> {
            let handle = self.registry.by_entity(player as i32)?;
            handle
                .held_item()
                .and_then(|item| item.vanilla_name().map(str::to_string))
        }

        /// A plugin asked to move a player. Resolved through the core
        /// safe-spot search (CMD-31) rather than an exact move, so a plugin
        /// cannot drop a player into lava, fire or the void — the design
        /// contract is that a plugin teleport is a *safe* teleport
        /// (`docs/design/plugin-sdk.md` §7.4).
        ///
        /// The SDK service layer already gates this on the caller's
        /// `players.teleport` capability; the check is repeated here as
        /// defense in depth, because a native plugin holds the backend and can
        /// call this directly.
        ///
        /// No [`crate::teleport::TeleportHooks`] impl is wired (the region
        /// index is unlanded and the WASM event world is frozen), so the region
        /// `entry` veto and the after-teleport observer are the extension
        /// point, not live behaviour — see KI 27.
        fn teleport_player(
            &self,
            plugin: &str,
            player: u64,
            x: f64,
            y: f64,
            z: f64,
            cause: TeleportCause,
        ) -> Result<(), String> {
            if !self
                .capabilities(plugin)
                .iter()
                .any(|c| c == aether_plugin::sdk::CAP_PLAYERS_TELEPORT)
            {
                return Err(format!(
                    "caller `{plugin}` lacks the `{}` capability",
                    aether_plugin::sdk::CAP_PLAYERS_TELEPORT
                ));
            }
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return Err("no such player".to_string());
            };
            let cause = match cause {
                TeleportCause::Command => crate::teleport::TeleportCause::Command,
                TeleportCause::Plugin => crate::teleport::TeleportCause::Plugin,
                TeleportCause::Portal => crate::teleport::TeleportCause::Portal,
            };
            let block = crate::teleport::BlockPos::new(
                x.floor() as i32,
                y.floor() as i32,
                z.floor() as i32,
            );
            crate::teleport::teleport(
                &handle,
                &self.registry,
                &*self.world,
                crate::teleport::TeleportTarget::Safe(block),
                cause,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        }

        /// An exact move, for a command that already validated the target
        /// (`/up`). Everything else goes through [`Self::teleport_player`],
        /// which resolves the CMD-31 safe spot. The capability gate is the
        /// same; a caller can never use this to bypass `players.teleport`.
        fn teleport_player_exact(
            &self,
            plugin: &str,
            player: u64,
            x: f64,
            y: f64,
            z: f64,
            cause: TeleportCause,
        ) -> Result<(), String> {
            if !self
                .capabilities(plugin)
                .iter()
                .any(|c| c == aether_plugin::sdk::CAP_PLAYERS_TELEPORT)
            {
                return Err(format!(
                    "caller `{plugin}` lacks the `{}` capability",
                    aether_plugin::sdk::CAP_PLAYERS_TELEPORT
                ));
            }
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return Err("no such player".to_string());
            };
            let cause = match cause {
                TeleportCause::Command => crate::teleport::TeleportCause::Command,
                TeleportCause::Plugin => crate::teleport::TeleportCause::Plugin,
                TeleportCause::Portal => crate::teleport::TeleportCause::Portal,
            };
            let pos = handle.pos();
            crate::teleport::teleport(
                &handle,
                &self.registry,
                &*self.world,
                crate::teleport::TeleportTarget::Exact(
                    aether_api::Vector3::new(x, y, z),
                    pos.yaw,
                    pos.pitch,
                ),
                cause,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        }

        fn disconnect_player(&self, _plugin: &str, player: u64, reason: &str) {
            if let Some(handle) = self.registry.by_entity(player as i32) {
                handle.disconnect(reason);
            }
        }

        fn online_players(&self, _plugin: &str) -> Vec<Player> {
            self.registry
                .snapshot()
                .iter()
                .map(|h| self.player(h))
                .collect()
        }

        /// The core command registry, so a command registered through the SDK's
        /// builder is dispatched and permission-checked like a core one. The
        /// ownership and duplicate rules live in the registry.
        fn register_command(
            &self,
            plugin: &str,
            name: &str,
            node: &str,
            op: u8,
        ) -> Result<(), String> {
            crate::commands::registry::register_plugin(name, plugin, node, op)
        }

        /// The effective capabilities the host granted `plugin`, recorded at
        /// load. The SDK's service layer uses this as its single capability
        /// gate.
        fn capabilities(&self, plugin: &str) -> Vec<String> {
            self.granted
                .lock()
                .unwrap()
                .get(plugin)
                .cloned()
                .unwrap_or_default()
        }

        // --- virtual menus --------------------------------------------------

        fn open_menu(
            &self,
            plugin: &str,
            player: u64,
            menu: u64,
            title: &str,
            layout: aether_plugin::menus::MenuLayout,
            items: &[aether_plugin::menus::MenuItem],
        ) -> bool {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return false;
            };
            let shape = menu_shape(layout);
            let mut slots: Vec<Option<crate::inventory::Stack>> = vec![None; shape.slot_count()];
            for it in items {
                if let Some(cell) = slots.get_mut(it.slot as usize) {
                    *cell = menu_stack(it);
                }
            }
            // The owner is recorded with the window, so a later click is
            // delivered only to the plugin that opened it.
            crate::game::window::open_menu(&handle, &self.world, plugin, menu, shape, title, slots);
            true
        }

        fn set_menu_slot(
            &self,
            _plugin: &str,
            player: u64,
            menu: u64,
            slot: u32,
            item: Option<&aether_plugin::menus::MenuItem>,
        ) -> bool {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return false;
            };
            // Only update a menu that is still the one open; a late update must
            // not touch whatever window replaced it.
            if crate::game::window::open_menu_id(&handle) != Some(menu) {
                return false;
            }
            let stack = item.and_then(menu_stack);
            crate::game::window::set_menu_slot(&handle, &self.world, slot as usize, stack);
            true
        }

        fn close_menu(&self, _plugin: &str, player: u64, menu: u64) {
            if let Some(handle) = self.registry.by_entity(player as i32) {
                crate::game::window::close_menu(&handle, &self.world, menu);
            }
        }

        /// Play a menu sound to one player, through the core sound event. The
        /// name is taken verbatim (a bare name gets `minecraft:`), so a plugin
        /// can name any sound event the client knows.
        fn play_sound(&self, _plugin: &str, player: u64, name: &str) {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return;
            };
            let p = handle.pos();
            handle.emit(
                &crate::protocol::ServerEvent::sound_at(name.to_string(), p.x, p.y, p.z),
                &*self.world,
            );
        }

        /// Give items through the core's own `give_or_drop`, so a menu `[give]`
        /// behaves exactly like `/give`: whatever does not fit is dropped,
        /// owned, at the player's feet and journalled. Returns the leftover
        /// count (0 on success); an unknown item name gives nothing.
        fn give_item(&self, _plugin: &str, player: u64, item: &str, count: u32) -> u64 {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return count as u64;
            };
            let Some(id) = aether_world::ItemTypeId::vanilla(item) else {
                return count as u64;
            };
            let (_taken, left) =
                crate::ground::give_or_drop(&handle, &self.registry, &self.world, id, count as u64);
            left
        }

        /// Record a command to run after the host lock is released. It is
        /// **not** dispatched here: dispatching inline would re-enter `HOST`
        /// (a `[command]` reaching `plugins::on_command` locks it again on the
        /// same thread) and deadlock the tick. `menu_click` drains the queue.
        fn run_command(&self, plugin: &str, player: Option<u64>, command: &str) -> Vec<String> {
            self.pending.lock().unwrap().push(DeferredCommand {
                plugin: plugin.to_string(),
                player,
                command: command.to_string(),
            });
            Vec::new()
        }

        /// The permission core, fronted for the SDK's `permissions` service.
        /// The player is looked up by session id, so address-bound operators
        /// are judged from the address they are really connected from; a
        /// player the registry does not know fails closed.
        fn permission_check(&self, _plugin: &str, id: u64, node: &str, op: u8) -> bool {
            match self.by_session(id) {
                Some(h) => crate::permissions::check_for(&h.name, h.ip, node, op),
                None => false,
            }
        }

        fn permission_level(&self, _plugin: &str, id: u64) -> u8 {
            self.by_session(id)
                .map(|h| crate::permissions::level(&h.name, h.ip).get())
                .unwrap_or(0)
        }

        fn permission_groups(&self, _plugin: &str, id: u64) -> Vec<String> {
            self.by_session(id)
                .map(|h| crate::permissions::groups_of(&h.name, h.ip))
                .unwrap_or_default()
        }

        fn permission_meta(&self, _plugin: &str, id: u64, key: &str) -> Option<String> {
            self.by_session(id)
                .and_then(|h| crate::permissions::meta(&h.name, h.ip, key))
        }

        fn permission_grant(
            &self,
            _plugin: &str,
            subject: &str,
            node: &str,
            value: bool,
        ) -> Result<(), String> {
            crate::permissions::set_node(subject, node, None, value)
        }

        fn permission_revoke(
            &self,
            _plugin: &str,
            subject: &str,
            node: &str,
        ) -> Result<(), String> {
            crate::permissions::unset_node(subject, node, None)
        }

        // --- History API (PA2) ------------------------------------------
        //
        // The SDK `history` service's provider forwards here; the journal
        // queries live in `aether-world` and this is only the mapping from
        // their compact ids to the plugin-facing records.

        fn history_block(
            &self,
            _plugin: &str,
            x: i32,
            y: i32,
            z: i32,
            before: Option<u64>,
            limit: u32,
        ) -> HistoryPage {
            page_of(
                &self.world,
                self.world
                    .journal()
                    .block_history(x, y, z, before, limit as usize),
            )
        }

        fn history_worldgen_original(&self, _plugin: &str, x: i32, y: i32, z: i32) -> bool {
            self.world
                .journal()
                .is_worldgen_original(x, y, z)
                .unwrap_or(false)
        }

        fn history_events(
            &self,
            _plugin: &str,
            window: HistoryWindow,
            before: Option<u64>,
            limit: u32,
        ) -> HistoryPage {
            let Some(window) = to_window(&window) else {
                // A kind name this build does not know selects nothing rather
                // than silently widening the query.
                return empty_page();
            };
            page_of(
                &self.world,
                self.world
                    .journal()
                    .history_page(&window, before, limit as usize),
            )
        }

        fn history_item_chain(
            &self,
            _plugin: &str,
            uid: u64,
            depth: u32,
            before: Option<u64>,
            limit: u32,
        ) -> HistoryPage {
            page_of(
                &self.world,
                self.world.journal().item_chain(
                    journal::ItemUid(uid),
                    depth as usize,
                    before,
                    limit as usize,
                ),
            )
        }

        fn history_container_log(
            &self,
            _plugin: &str,
            x: i32,
            y: i32,
            z: i32,
            before: Option<u64>,
            limit: u32,
        ) -> HistoryPage {
            page_of(
                &self.world,
                self.world
                    .journal()
                    .container_log(x, y, z, before, limit as usize),
            )
        }

        fn history_player_events(
            &self,
            _plugin: &str,
            actor_low: u64,
            actor_high: u64,
            before: Option<u64>,
            limit: u32,
        ) -> HistoryPage {
            let actor = join_uuid(actor_low, actor_high);
            page_of(
                &self.world,
                self.world
                    .journal()
                    .player_events(actor, before, limit as usize),
            )
        }

        fn set_sidebar(
            &self,
            _plugin: &str,
            player: u64,
            board: &aether_plugin::sidebar::SidebarBoard,
        ) {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return;
            };
            let board = crate::protocol::scoreboard::SidebarBoard::new(
                board.title.clone(),
                board.lines.clone(),
            );
            handle.update_sidebar(Some(&board), &*self.world);
        }

        fn clear_sidebar(&self, _plugin: &str, player: u64) {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return;
            };
            handle.update_sidebar(None, &*self.world);
        }

        fn set_tab_header_footer(&self, _plugin: &str, player: u64, header: &str, footer: &str) {
            let Some(handle) = self.registry.by_entity(player as i32) else {
                return;
            };
            handle.set_tab_header_footer(header, footer, &*self.world);
        }

        fn player_extra(
            &self,
            _plugin: &str,
            player: u64,
        ) -> Option<aether_plugin::placeholders::PlayerExtra> {
            let handle = self.registry.by_entity(player as i32)?;
            let pos = handle.pos();
            let (health, _food) = {
                let game = handle.game();
                (game.health, game.food)
            };
            Some(aether_plugin::placeholders::PlayerExtra {
                // The measured keep-alive round trip; `0` until the first
                // reply.
                ping: handle.ping() as i32,
                world: "minecraft:overworld".to_string(),
                x: pos.x,
                y: pos.y,
                z: pos.z,
                health,
                level: 0,
                display_name: handle.name.clone(),
            })
        }

        fn max_players(&self, _plugin: &str) -> u32 {
            self.config.server.max_players
        }

        fn server_tps(&self, _plugin: &str) -> f64 {
            // The tick loop's measured rate.
            crate::game::tps()
        }

        fn world_time(&self, _plugin: &str) -> i64 {
            crate::game::time_of_day()
        }

        fn vanilla_sidebar_claimed(&self, _plugin: &str, _player: u64) -> bool {
            // The vanilla scoreboard model (CMD-15) is not implemented yet, so
            // no `/scoreboard objectives setdisplay sidebar` can have claimed
            // the slot. When it lands, this reads that model.
            false
        }

        // --- Staff accountability (the native `staff` plugin) --------------

        fn staff_set_vanished(&self, _plugin: &str, player: u64, vanished: bool) {
            if let Some(handle) = self.registry.by_entity(player as i32) {
                crate::staff::set_vanished(&handle, vanished, &self.registry, &*self.world);
            }
        }

        fn staff_is_vanished(&self, _plugin: &str, player: u64) -> bool {
            self.registry
                .by_entity(player as i32)
                .is_some_and(|h| h.staff().vanished())
        }

        fn staff_set_sees_vanished(&self, _plugin: &str, player: u64, see: bool) {
            if let Some(handle) = self.registry.by_entity(player as i32) {
                crate::staff::set_sees_vanished(&handle, see, &self.registry, &*self.world);
            }
        }

        fn staff_is_spectating(&self, _plugin: &str, player: u64) -> bool {
            self.registry
                .by_entity(player as i32)
                .is_some_and(|h| h.staff().spectating())
        }

        fn staff_enter_spectate(
            &self,
            _plugin: &str,
            player: u64,
            target: Option<u64>,
        ) -> Result<(), String> {
            let handle = self
                .registry
                .by_entity(player as i32)
                .ok_or("that player is not online")?;
            let target = match target {
                Some(t) => Some(
                    self.registry
                        .by_entity(t as i32)
                        .ok_or("that target is not online")?,
                ),
                None => None,
            };
            crate::staff::enter_spectate(&handle, &self.registry, &self.world, target.as_deref())
        }

        fn staff_leave_spectate(&self, _plugin: &str, player: u64) -> Result<(), String> {
            let handle = self
                .registry
                .by_entity(player as i32)
                .ok_or("that player is not online")?;
            crate::staff::leave_spectate(&handle, &self.registry, &self.world)
        }

        fn staff_set_xray_level(
            &self,
            _plugin: &str,
            player: u64,
            level: &str,
        ) -> Result<(), String> {
            let handle = self
                .registry
                .by_entity(player as i32)
                .ok_or("that player is not online")?;
            let parsed = crate::staff::AntiXrayLevel::parse(level).ok_or_else(|| {
                format!("`{level}` is not a level — use off, engine, culling or a number")
            })?;
            crate::staff::set_xray_level(&handle, parsed, &*self.world);
            Ok(())
        }

        fn staff_actionbar(&self, _plugin: &str, player: u64, text: &str) {
            if let Some(handle) = self.registry.by_entity(player as i32) {
                handle.emit(
                    &crate::protocol::ServerEvent::ActionBar(text.to_string()),
                    &*self.world,
                );
            }
        }

        fn staff_journal(&self, _plugin: &str, actor: u64, action: &str, detail: &str) {
            // `actor` is the acting player's entity id; the journal keys by
            // UUID, so resolve it here. An unknown player (gone already) is the
            // server actor, which is honest rather than inventing an identity.
            let uuid = self
                .registry
                .by_entity(actor as i32)
                .map(|h| h.uuid)
                .unwrap_or(0);
            // A plugin-defined journal event under the staff tag; the payload
            // is `action\x1fdetail`, opaque to the core.
            let mut bytes = Vec::with_capacity(action.len() + detail.len() + 1);
            bytes.extend_from_slice(action.as_bytes());
            bytes.push(0x1f);
            bytes.extend_from_slice(detail.as_bytes());
            let _ = self.world.journal().append(
                aether_api::JournalActor(uuid),
                aether_world::journal::EventBody::Custom {
                    tag: STAFF_JOURNAL_TAG,
                    bytes,
                },
            );
        }

        // --- Regions and claims ---------------------------------------------

        /// Insert or replace a region owned by `plugin`.
        ///
        /// The host stamps `plugin` as the source. The ownership test and the
        /// write are atomic in the store, so a plugin can never overwrite a
        /// core-created region (`source: None`) or another plugin's region,
        /// even if the numeric id collides.
        fn region_upsert(
            &self,
            plugin: &str,
            info: &aether_plugin::RegionInfo,
        ) -> std::result::Result<(), aether_plugin::RegionWriteError> {
            let region = core_region(plugin, info);
            crate::region::store()
                .upsert_owned(plugin, region)
                .map_err(map_region_error)
        }

        /// Atomically insert a claim-style region: refuses when a region from
        /// the same plugin already overlaps the box. Regions from other plugins
        /// may overlap and are decided by the core's priority chain.
        fn region_insert(
            &self,
            plugin: &str,
            info: &aether_plugin::RegionInfo,
        ) -> std::result::Result<(), aether_plugin::RegionWriteError> {
            let region = core_region(plugin, info);
            crate::region::store()
                .insert_new_owned(plugin, region, |existing| {
                    existing.source.as_deref() == Some(plugin)
                })
                .map_err(map_region_error)
        }

        /// Remove a region, but only when `plugin` owns it. Atomic in the
        /// store; a missing, core-owned or foreign region is left alone.
        fn region_remove(&self, plugin: &str, id: u64) {
            crate::region::store().remove_owned(plugin, crate::region::RegionId(id));
        }

        fn region_get(&self, _plugin: &str, id: u64) -> Option<aether_plugin::RegionInfo> {
            crate::region::store()
                .snapshot()
                .get(crate::region::RegionId(id))
                .map(|region| region_info(region))
        }

        fn regions_overlapping(
            &self,
            _plugin: &str,
            world: &str,
            min: [i32; 3],
            max: [i32; 3],
        ) -> Vec<aether_plugin::RegionInfo> {
            let index = crate::region::store().snapshot();
            index
                .overlapping_box(
                    world,
                    crate::region::BlockPos::new(min[0], min[1], min[2]),
                    crate::region::BlockPos::new(max[0], max[1], max[2]),
                )
                .into_iter()
                .map(|region| region_info(region))
                .collect()
        }
    }

    /// The core region a plugin write describes, with the caller as its source.
    fn core_region(plugin: &str, info: &aether_plugin::RegionInfo) -> crate::region::Region {
        let mut region = crate::region::Region::new(
            crate::region::RegionId(info.id),
            info.world.clone(),
            crate::region::BlockPos::new(info.min[0], info.min[1], info.min[2]),
            crate::region::BlockPos::new(info.max[0], info.max[1], info.max[2]),
        );
        region.priority = info.priority;
        region.source = Some(plugin.to_string());
        for (name, allowed) in &info.flags {
            region.flags.set(name.clone(), *allowed);
        }
        region.members = info.members.iter().cloned().collect();
        region.owners = info.owners.iter().cloned().collect();
        region.trusted = info
            .trusted
            .iter()
            .map(|(name, flags)| (name.clone(), flags.iter().cloned().collect()))
            .collect();
        region
    }

    /// The plugin-facing error for a refused core write.
    fn map_region_error(error: crate::region::RegionEditError) -> aether_plugin::RegionWriteError {
        use crate::region::RegionEditError;
        match error {
            RegionEditError::OwnedByCore => aether_plugin::RegionWriteError::OwnedByCore,
            RegionEditError::OwnedByOther(source) => {
                aether_plugin::RegionWriteError::OwnedByOther(source)
            }
            RegionEditError::IdTaken => aether_plugin::RegionWriteError::IdTaken,
            RegionEditError::Overlap => aether_plugin::RegionWriteError::Overlap,
        }
    }

    /// The plugin-facing view of a core region.
    fn region_info(region: &crate::region::Region) -> aether_plugin::RegionInfo {
        aether_plugin::RegionInfo {
            id: region.id.get(),
            source: region.source.clone(),
            world: region.world.clone(),
            min: [region.min.x, region.min.y, region.min.z],
            max: [region.max.x, region.max.y, region.max.z],
            priority: region.priority,
            flags: region
                .flags
                .iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            members: region.members.iter().cloned().collect(),
            owners: region.owners.iter().cloned().collect(),
            trusted: region
                .trusted
                .iter()
                .map(|(name, flags)| (name.clone(), flags.iter().cloned().collect()))
                .collect(),
        }
    }

    /// The custom journal tag the staff plugin's audit events use.
    pub const STAFF_JOURNAL_TAG: u8 = aether_world::journal::event::TAG_CUSTOM_MIN;

    /// Dispatch every command a host callback deferred, once no host lock is
    /// held. Player commands go through the core dispatcher (the same gate as
    /// any typed command); `[console]` has no dispatcher yet, so it is logged
    /// and dropped rather than silently run as the player.
    fn drain_deferred(backend: &Arc<ServerBackend>) {
        let pending: Vec<DeferredCommand> = {
            let mut q = backend.pending.lock().unwrap();
            std::mem::take(&mut *q)
        };
        for cmd in pending {
            let DeferredCommand {
                plugin,
                player,
                command,
            } = cmd;
            let Some(id) = player else {
                crate::log::warn(&format!(
                    "plugin `{plugin}`: `[console] {command}` ignored (no console dispatch yet)"
                ));
                continue;
            };
            let Some(handle) = backend.registry.by_entity(id as i32) else {
                continue;
            };
            let text = if command.starts_with('/') {
                command
            } else {
                format!("/{command}")
            };
            if let Some(feedback) =
                crate::commands::dispatch(&text, &handle, &backend.registry, &backend.world)
            {
                let source = crate::commands::source::CommandSource::Player(&handle);
                let rules = crate::commands::feedback::GameRules::load(&backend.world);
                crate::commands::feedback::send(&source, &feedback, rules, |line| {
                    handle.emit(&ServerEvent::Chat(line.to_string()), &*backend.world)
                });
            }
        }
    }

    /// An empty plugin-facing page.
    fn empty_page() -> HistoryPage {
        HistoryPage {
            events: Vec::new(),
            next: None,
        }
    }

    /// Map a journal page to the plugin-facing one.
    fn page_of(
        world: &DemoWorld,
        page: Result<journal::Page, aether_world::StorageError>,
    ) -> HistoryPage {
        match page {
            Ok(p) => HistoryPage {
                events: p.events.iter().map(|e| to_event(world, e)).collect(),
                next: p.next,
            },
            Err(e) => {
                crate::log::warn(&format!("history query failed: {e}"));
                empty_page()
            }
        }
    }

    /// Map a neutral menu layout onto the server's window shape.
    fn menu_shape(layout: aether_plugin::menus::MenuLayout) -> crate::game::player::MenuShape {
        use crate::game::player::MenuShape as S;
        use aether_plugin::menus::MenuLayout as L;
        match layout {
            L::Chest1 => S::Chest1,
            L::Chest2 => S::Chest2,
            L::Chest3 => S::Chest3,
            L::Chest4 => S::Chest4,
            L::Chest5 => S::Chest5,
            L::Chest6 => S::Chest6,
            L::Dispenser => S::Dispenser,
            L::Hopper => S::Hopper,
        }
    }

    /// Translate a neutral menu item into a display-only server stack, or
    /// `None` for an empty slot. An item the world's registry does not know is
    /// drawn as a barrier rather than dropped, so a slot never silently
    /// vanishes and shifts the menu around it.
    fn menu_stack(item: &aether_plugin::menus::MenuItem) -> Option<crate::inventory::Stack> {
        if item.item.is_empty() || item.count == 0 {
            return None;
        }
        let id = match aether_world::ItemTypeId::vanilla(&item.item) {
            Some(id) => id,
            None => crate::inventory::item("minecraft:barrier"),
        };
        let count = item.count.clamp(1, 64) as u8;
        Some(crate::inventory::Stack::menu_item(
            id,
            count,
            item.name.clone(),
            item.lore.clone(),
            item.glint,
            item.model,
        ))
    }

    /// A UUID from the two halves the component model carries.
    fn join_uuid(low: u64, high: u64) -> ActorId {
        ActorId(((high as u128) << 64) | (low as u128))
    }

    /// A journal window from the plugin-facing one, or `None` when it names an
    /// unknown event kind.
    fn to_window(w: &HistoryWindow) -> Option<journal::Window> {
        let mut window = journal::Window::new();
        if w.has_actor {
            window = window.actor(join_uuid(w.actor_low, w.actor_high));
        }
        if w.has_area {
            window = window.area(journal::Area::between(
                (w.x1, w.y1, w.z1),
                (w.x2, w.y2, w.z2),
            ));
        }
        window = window.since(w.since_ms).until(w.until_ms);
        if !w.kinds.is_empty() {
            let mut tags = Vec::with_capacity(w.kinds.len());
            for name in &w.kinds {
                let (tag, _) = journal::EVENT_KINDS
                    .iter()
                    .find(|(_, n)| *n == name.as_str())?;
                tags.push(*tag);
            }
            window = window.kinds(&tags);
        }
        Some(window)
    }

    /// Render a place the way a plugin sees it.
    fn place_name(p: &Place) -> String {
        match p {
            Place::Inventory { slot, .. } => format!("inventory:{slot}"),
            Place::Ground { x, y, z } => format!("ground:{x},{y},{z}"),
            Place::Container { x, y, z, slot } => format!("container:{x},{y},{z}:{slot}"),
            Place::Nowhere => "nowhere".to_string(),
        }
    }

    /// One journal event as the plugin-facing record.
    fn to_event(world: &DemoWorld, e: &Event) -> HistoryEvent {
        let uuid = e.actor.0;
        let mut out = HistoryEvent {
            seq: e.seq,
            at_ms: e.at_ms,
            actor_low: uuid as u64,
            actor_high: (uuid >> 64) as u64,
            kind: e.body.kind_name().to_string(),
            pos: e.position().map(|(x, y, z)| HistoryPos { x, y, z }),
            from_block: None,
            to_block: None,
            uid: None,
            item: None,
            count: None,
            from_place: None,
            to_place: None,
            reason: None,
            cause: e.cause,
            sources: Vec::new(),
        };
        match &e.body {
            EventBody::BlockSet { from, to, .. } => {
                out.from_block = world.block_name_of(*from);
                out.to_block = world.block_name_of(*to);
            }
            EventBody::ItemMint {
                uid,
                item,
                count,
                to,
            } => {
                out.uid = Some(uid.0);
                out.item = world.item_name(*item);
                out.count = Some(*count);
                out.to_place = Some(place_name(to));
            }
            EventBody::ItemMove {
                uid,
                from,
                to,
                count,
            } => {
                out.uid = Some(uid.0);
                out.count = Some(*count);
                out.from_place = Some(place_name(from));
                out.to_place = Some(place_name(to));
            }
            EventBody::ItemDestroy { uid, from, reason } => {
                out.uid = Some(uid.0);
                out.from_place = Some(place_name(from));
                out.reason = Some(reason.name().to_string());
            }
            EventBody::ItemMerge { into, from, count } => {
                out.uid = Some(into.0);
                out.count = Some(*count);
                out.sources = vec![from.0];
            }
            EventBody::ItemDerive {
                uid,
                item,
                count,
                to,
                from,
            } => {
                out.uid = Some(uid.0);
                out.item = world.item_name(*item);
                out.count = Some(*count);
                out.to_place = Some(place_name(to));
                out.sources = from.iter().map(|u| u.0).collect();
            }
            // A player-text event carries no block/item side; its `kind`
            // (chat/pm/command) and position are already on the record.
            EventBody::ChatMessage { .. } => {}
            // A bulk revision is many block changes under one event. The
            // plugin record has a single block/item side, so it carries none;
            // the touched positions are in the journal, not this summary.
            EventBody::BlockGroup { .. } => {}
            EventBody::Custom { .. } => {}
        }
        out
    }

    static HOST: OnceLock<Mutex<PluginHost>> = OnceLock::new();
    /// The concrete backend, so a menu click can drain deferred commands
    /// outside the host lock (the [`PluginHost`] holds it only as
    /// `Arc<dyn HostBackend>` and cannot reach the concrete queue).
    static BACKEND: OnceLock<Arc<ServerBackend>> = OnceLock::new();

    /// Load every plugin the operator configured. Called once at startup.
    pub fn install(cfg: &Arc<Config>, world: &Arc<DemoWorld>, registry: &SharedRegistry) {
        if cfg.plugins.dir.trim().is_empty() {
            return;
        }
        let backend = Arc::new(ServerBackend {
            world: Arc::clone(world),
            config: Arc::clone(cfg),
            registry: Arc::clone(registry),
            granted: Mutex::new(std::collections::BTreeMap::new()),
            pending: Mutex::new(Vec::new()),
        });
        // Kept so a menu click can drain the deferred-command queue **after**
        // the host lock is released; the host itself cannot reach the
        // concrete backend to do that.
        let _ = BACKEND.set(Arc::clone(&backend));
        // Kept so each plugin's effective grant can be recorded for the SDK's
        // capability gate; `host` takes ownership of the other handle.
        let grant_sink = Arc::clone(&backend);
        let host_config = HostConfig {
            budget_ms: cfg.plugins.budget_ms.max(1),
            tick_budget_ms: cfg.plugins.tick_budget_ms.max(1),
            memory_mib: cfg.plugins.memory_mib.max(1),
            max_faults: cfg.plugins.max_faults.max(1),
            init_fuel: cfg.plugins.init_fuel.max(1),
            // Every core name — implemented and specified-but-unimplemented —
            // so the host itself refuses to route one to a plugin, in addition
            // to the registry's ownership check at registration.
            reserved_commands: reserved_command_names(),
        };
        let mut host = match PluginHost::new(host_config, backend) {
            Ok(h) => h,
            Err(e) => {
                crate::log::warn(&format!("plugins disabled: {e}"));
                return;
            }
        };
        // Native plugins first, so a dependent's `requires` are satisfied
        // before it loads. The SDK is the only one today; it is a plugin like
        // any other, disabled by `[plugins.sdk] enabled = false`, and a
        // dependent that needs it then refuses to load with a clear error.
        //
        // The live-ops service the web admin reads is published before the
        // native plugins, so `webadmin`'s `requires = ["ops@0.1"]` resolves.
        let ops = Arc::new(aether_ops::OpsHandle(Arc::new(crate::web::ServerOps::new(
            Arc::clone(world),
            Arc::clone(registry),
            Arc::clone(cfg),
        ))));
        if let Err(e) = host
            .services()
            .register(aether_ops::SERVICE, aether_ops::VERSION, ops)
        {
            crate::log::warn(&format!("ops service not registered: {e}"));
        }
        // The operator may lower the bulk-edit cap for their server. The
        // plugin's own pre-check reads the same value through the host
        // service, so both the refusal message and the `BulkEdit` limit agree.
        let worldedit_max = cfg
            .plugins
            .plugins
            .get(aether_worldedit_plugin::ID)
            .and_then(|entry| {
                entry
                    .config
                    .get("max-volume")
                    .or_else(|| entry.config.get("max_volume"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(crate::bulk::DEFAULT_MAX_BLOCKS);
        let schematic_dir = std::path::Path::new(&cfg.server.world_dir).join("schematics");
        let worldedit = Arc::new(crate::worldedit::Host::with_schematics(
            Arc::clone(world),
            Arc::clone(registry),
            worldedit_max,
            Some(schematic_dir),
        ));
        if let Err(e) = aether_plugin::worldedit::register_host(host.services(), worldedit) {
            crate::log::warn(&format!("worldedit service not registered: {e}"));
        }
        install_native(&mut host, cfg, &grant_sink, world);
        for file in discover(Path::new(&cfg.plugins.dir)) {
            let id = file.manifest.id.clone();
            // A native plugin is compiled in and loaded from its build-time
            // registration above; a plugin.toml beside its source is only
            // documentation. Discovering it again would look like a duplicate.
            if file.manifest.tier == aether_plugin::PluginTier::Native {
                crate::log::info(&format!("plugin `{id}` is native; loaded from the build"));
                continue;
            }
            let entry = cfg.plugins.plugins.get(&id).cloned().unwrap_or_default();
            if !entry.enabled() {
                crate::log::info(&format!("plugin `{id}` disabled by config"));
                continue;
            }
            let caps = effective_capabilities(&file.manifest.capabilities, &entry.capabilities);
            match host.add(&file, &caps, &entry.config) {
                Ok(()) => {
                    grant_sink
                        .granted
                        .lock()
                        .unwrap()
                        .insert(id.clone(), caps.clone());
                    // Register the plugin's commands through the command
                    // registry, with the ownership and permission model the
                    // core uses; a core name is refused here, not shadowed.
                    register_commands(&file.manifest);
                    crate::log::info(&format!(
                        "plugin     : {} v{} (priority {}, {} event(s), {} command(s))",
                        id,
                        file.manifest.version,
                        file.manifest.priority,
                        file.manifest.subscribe.len(),
                        file.manifest.commands.len()
                    ));
                }
                Err(e) => crate::log::warn(&format!("plugin `{id}` not loaded: {e}")),
            }
        }
        // Nothing loaded: leave the host uninstalled so no event takes the
        // process-wide plugin lock and no epoch ticker runs for nothing.
        if host.ids().is_empty() {
            crate::log::info("plugin     : none active");
            return;
        }
        let _ = HOST.set(Mutex::new(host));
    }

    /// The requested capabilities, narrowed by what the operator granted.
    ///
    /// **Default is deny.** A plugin receives a capability only if it both
    /// asked for it and the operator listed it under `[plugins.<id>]`. Just
    /// pointing `[plugins] dir` at a folder is not a grant, so a dropped-in
    /// plugin cannot silently acquire one — the design's "requests *and* the
    /// operator grants".
    fn effective_capabilities(requested: &[String], granted: &[String]) -> Vec<String> {
        requested
            .iter()
            .filter(|c| granted.iter().any(|g| g == *c))
            .cloned()
            .collect()
    }

    /// Add the compiled-in native plugins, in dependency order: the SDK first
    /// (so the sidebar's optional permission checks and the web admin's
    /// `requires` resolve), then the sidebar, then the admin web interface.
    ///
    /// A native plugin is a plugin like any other — same manifest, enable
    /// flag and load error handling — except that it is compiled in and
    /// reaches the host through the in-process [`PluginHost::add_native`]
    /// rather than a `.wasm` file. Loading them before the on-disk discovery is
    /// what lets a WASM dependent's `requires` be satisfied.
    fn install_native(
        host: &mut PluginHost,
        cfg: &Arc<Config>,
        grant_sink: &Arc<ServerBackend>,
        world: &Arc<DemoWorld>,
    ) {
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_sdk_plugin::registration(),
            "native SDK",
        );
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_sidebar_plugin::registration(),
            "native sidebar",
        );
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_voice_plugin::registration(),
            "native voice",
        );
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_menus_plugin::registration(),
            "native menus",
        );
        // The staff plugin is independent of the SDK; it self-registers its
        // commands and drives the core's `staff_*` hooks.
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_staff_plugin::registration(),
            "native staff",
        );
        // The regions plugin edits the process-wide `aether-region` store the
        // protection hooks read, so it is independent of the SDK as well.
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_regions_plugin::registration(),
            "native regions",
        );
        // Claims fill the same core region index through the `region_*` host
        // hooks; the protection check itself stays in the core (CMD-29b).
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_claims_plugin::registration(),
            "native claims",
        );
        // Essentials homes/warps/spawn: no core mechanism of its own, every
        // move goes through the CMD-31 teleport service and state lives in
        // plugin KV. `/spawn` remains core, so this registers only new names.
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_essentials_teleport_plugin::registration(),
            "native essentials-teleport",
        );
        install_native_one(
            host,
            cfg,
            grant_sink,
            aether_worldedit_plugin::registration(),
            "native worldedit",
        );

        // The admin web interface is the one native plugin that needs the live
        // world, which the server attaches here — the plugin host gives a
        // native plugin no world handle, by design. It also resolves the
        // live-ops service, published by `install` above.
        let id = "webadmin".to_string();
        let entry = cfg.plugins.plugins.get(&id).cloned().unwrap_or_default();
        if !entry.enabled() {
            crate::log::info(&format!("plugin `{id}` disabled by config"));
            return;
        }
        let registration = aether_webadmin::registration();
        let caps = effective_capabilities(&registration.manifest.capabilities, &entry.capabilities);
        let file = ManifestFile {
            dir: std::path::PathBuf::new(),
            manifest: registration.manifest,
        };
        let plugin = Box::new(aether_webadmin::WebAdminPlugin::with_world(Arc::clone(
            world,
        )));
        match host.add_native(&file, &caps, &entry.config, plugin) {
            Ok(()) => {
                grant_sink
                    .granted
                    .lock()
                    .unwrap()
                    .insert(id.clone(), caps.clone());
                crate::log::info(&format!("plugin     : {id} (native web admin)"));
            }
            Err(e) => crate::log::warn(&format!("plugin `{id}` not loaded: {e}")),
        }
    }

    /// Load one compiled-in native plugin from its build-time registration.
    fn install_native_one(
        host: &mut PluginHost,
        cfg: &Arc<Config>,
        grant_sink: &Arc<ServerBackend>,
        registration: aether_plugin::NativeRegistration,
        what: &str,
    ) {
        let id = registration.manifest.id.clone();
        let entry = cfg.plugins.plugins.get(&id).cloned().unwrap_or_default();
        if !entry.enabled() {
            crate::log::info(&format!("plugin `{id}` disabled by config"));
            return;
        }
        // Native plugins ask the operator for no host capabilities of their
        // own; a service method still checks its *caller's* capabilities.
        let caps = effective_capabilities(&registration.manifest.capabilities, &entry.capabilities);
        let file = ManifestFile {
            // Native plugins are compiled in; there is no directory.
            dir: std::path::PathBuf::new(),
            manifest: registration.manifest,
        };
        match host.add_native(&file, &caps, &entry.config, (registration.new)()) {
            Ok(()) => {
                grant_sink
                    .granted
                    .lock()
                    .unwrap()
                    .insert(id.clone(), caps.clone());
                // Register the native plugin's commands, exactly as for a
                // WASM plugin, so `/sidebar` and `/voice` are dispatched and
                // permission-checked through the core registry. The same
                // ownership check as any other plugin applies.
                register_commands(&file.manifest);
                crate::log::info(&format!("plugin     : {id} ({what})"));
            }
            Err(e) => {
                // A protection plugin that failed to load leaves the shared
                // region store empty, so every region stops protecting. Say so
                // at error level instead of a warning lost among plugin noise;
                // the state bytes are intact, but the operator must act.
                if id == aether_regions_plugin::ID {
                    crate::log::error(&format!(
                        "plugin `{id}` not loaded: {e} — REGION PROTECTION IS NOT ACTIVE;                          fix the plugin storage and restart"
                    ));
                } else {
                    crate::log::warn(&format!("plugin `{id}` not loaded: {e}"));
                }
            }
        }
    }

    /// Register a loaded plugin's commands in the command registry, so they are
    /// dispatched and permission-checked the same way core commands are. A core
    /// name is refused (logged) rather than shadowed; the host's reserved set
    /// is the belt to this braces.
    fn register_commands(manifest: &aether_plugin::Manifest) {
        for decl in &manifest.commands {
            let node = decl.node(&manifest.id);
            if let Err(e) = crate::commands::registry::register_plugin(
                decl.name(),
                &manifest.id,
                &node,
                decl.op(),
            ) {
                crate::log::warn(&format!(
                    "plugin `{}`: command not registered: {e}",
                    manifest.id
                ));
            }
        }
    }

    /// Every core command name, implemented or specified: the set a plugin may
    /// never answer. Derived from the one catalogue, so it cannot drift.
    fn reserved_command_names() -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for c in crate::commands::catalogue::CATALOGUE {
            names.push(c.name.to_string());
            names.extend(c.aliases.iter().map(|a| (*a).to_string()));
        }
        names
    }

    // A per-test-thread host override, so a test can drive a plugin without
    // racing the process-wide `HOST` another test may install.
    #[cfg(test)]
    thread_local! {
        static TEST_HOST: std::cell::RefCell<Option<&'static Mutex<PluginHost>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Install `host` for the current test thread only.
    ///
    /// The `Box::leak` is deliberate: the slot is a `&'static` because the
    /// host must outlive the test's locals, and a test process is short-lived,
    /// so reclaiming one host per test is not worth a registry.
    #[cfg(test)]
    pub(crate) fn install_test_host(host: PluginHost) {
        let host: &'static Mutex<PluginHost> = Box::leak(Box::new(Mutex::new(host)));
        TEST_HOST.with(|slot| *slot.borrow_mut() = Some(host));
    }

    /// Call `f` with the host, if one is installed.
    fn with<R>(default: R, f: impl FnOnce(&mut PluginHost) -> R) -> R {
        #[cfg(test)]
        if let Some(lock) = TEST_HOST.with(|slot| *slot.borrow()) {
            return f(&mut lock.lock().unwrap());
        }
        match HOST.get() {
            Some(lock) => f(&mut lock.lock().unwrap()),
            None => default,
        }
    }

    fn player(handle: &PlayerHandle) -> Player {
        Player {
            id: handle.entity_id as u64,
            name: handle.name.clone(),
            // The server's own answer, not the client's claim: a plugin can
            // gate its own commands on this.
            operator: crate::is_operator(handle),
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

    /// A block was placed by a bulk edit (native plugins only: the frozen
    /// WASM world has no `after-block-place` export).
    pub fn after_block_place(handle: &PlayerHandle, x: i32, y: i32, z: i32, block: &str) {
        with((), |h| h.after_block_place(&player(handle), x, y, z, block));
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

    /// The player started mining a block, before the region check and before a
    /// survival dig begins. A native plugin's selection wand uses this so the
    /// point is set immediately, even on a protected or unbreakable block.
    pub fn before_block_dig(
        handle: &PlayerHandle,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Option<String> {
        with(None, |h| {
            cancelled(h.before_block_dig(&player(handle), x, y, z, block))
        })
    }

    /// A right-click on a block, for a native plugin's selection wand. A
    /// cancelled click does not open, toggle or place anything.
    pub fn before_block_interact(
        handle: &PlayerHandle,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Option<String> {
        with(None, |h| {
            cancelled(h.before_block_interact(&player(handle), x, y, z, block))
        })
    }

    /// A right-click on a block with `item`: the native golden-shovel hook.
    #[allow(clippy::too_many_arguments)]
    pub fn before_use_block(
        handle: &PlayerHandle,
        x: i32,
        y: i32,
        z: i32,
        item: &str,
        block: &str,
    ) -> Option<String> {
        with(None, |h| {
            cancelled(h.before_use_block(&player(handle), x, y, z, item, block))
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

    /// A click on a virtual menu: delivered to the plugin that owns it. The
    /// core has already guaranteed the click cannot move an item, so this only
    /// dispatches.
    ///
    /// The host lock is taken for the callback and **released before any
    /// command the callback asked to run is dispatched** — otherwise a
    /// `[command]` action would re-enter the host to reach a plugin command
    /// and hang the tick. See [`DeferredCommand`].
    pub fn menu_click(
        handle: &PlayerHandle,
        owner: &str,
        menu: u64,
        slot: i32,
        button: i8,
        mode: i32,
    ) {
        with((), |h| {
            h.menu_click(owner, &player(handle), menu, slot, button, mode)
        });
        if let Some(backend) = BACKEND.get() {
            drain_deferred(backend);
        }
    }

    /// A virtual menu was closed; tell the plugin that owns it.
    pub fn menu_close(handle: &PlayerHandle, owner: &str, menu: u64) {
        with((), |h| h.menu_close(owner, &player(handle), menu));
        if let Some(backend) = BACKEND.get() {
            drain_deferred(backend);
        }
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
    ///
    /// The **core command registry** is the source of truth for who owns a
    /// name, so the plugin is resolved from it and asked directly — a plugin
    /// that registered a command at runtime (a declarative menu's
    /// `open_commands`) cannot be found through a static manifest list.
    pub fn on_command(handle: &PlayerHandle, name: &str, args: &[String]) -> Option<Vec<String>> {
        let entry = crate::commands::registry::plugin_command(name)?;
        with(None, |h| {
            h.on_command_for(&entry.plugin, &player(handle), name, args)
                .map(|r| r.lines)
        })
    }

    /// A plugin-channel message from a player, forwarded to every native plugin
    /// subscribed to `on-plugin-message`.
    pub fn on_plugin_message(handle: &PlayerHandle, channel: &str, data: &[u8]) {
        with((), |h| h.on_plugin_message(&player(handle), channel, data));
    }

    pub fn shutdown() {
        with((), |h| h.shutdown());
    }

    #[cfg(test)]
    mod tests {
        use super::effective_capabilities;
        use aether_world::journal;

        #[test]
        fn capabilities_are_deny_by_default() {
            // The operator granted nothing: the plugin gets nothing, however
            // much it asked for.
            let requested = vec!["log".to_string(), "storage.kv".to_string()];
            assert!(
                effective_capabilities(&requested, &[]).is_empty(),
                "an ungranted capability must not be handed out"
            );
            // Granted: exactly the intersection.
            assert_eq!(
                effective_capabilities(
                    &["log".to_string(), "world.write".to_string()],
                    &["log".to_string()]
                ),
                vec!["log".to_string()]
            );
            // A grant the manifest never asked for is not forced on it.
            assert!(effective_capabilities(&[], &["log".to_string()]).is_empty());
        }

        #[test]
        fn a_core_region_round_trips_through_the_plugin_shape() {
            use crate::region::{BlockPos, Region, RegionId};

            let region = Region::new(
                RegionId(7),
                "minecraft:overworld",
                BlockPos::new(-1, -64, 2),
                BlockPos::new(16, 383, 18),
            )
            .with_priority(20)
            .with_source("claims")
            .with_flag("build", false)
            .owner("Alice")
            .member("Bob")
            .trusted("Carol", ["use".to_string()]);

            let info = super::region_info(&region);
            assert_eq!(info.id, 7);
            assert_eq!(info.source.as_deref(), Some("claims"));
            assert_eq!(info.min, [-1, -64, 2]);
            assert_eq!(info.max, [16, 383, 18]);
            assert_eq!(info.priority, 20);
            assert_eq!(info.flags.get("build"), Some(&false));
            assert_eq!(info.owners, vec!["Alice".to_string()]);
            assert_eq!(info.members, vec!["Bob".to_string()]);
            assert_eq!(
                info.trusted.get("Carol"),
                Some(&["use".to_string()].into_iter().collect())
            );
        }

        #[test]
        fn the_reserved_set_covers_core_and_specified_names() {
            // The host's reserved set is what stops a plugin being routed a
            // core name; it must include the implemented ones and the
            // specified-but-unimplemented stubs alike.
            let names = super::reserved_command_names();
            assert!(names.iter().any(|n| n == "give"), "implemented core name");
            assert!(names.iter().any(|n| n == "stop"), "CMD-09 stub");
            assert!(names.iter().any(|n| n == "xp"), "a stub alias");
            assert!(!names.iter().any(|n| n == "hello"));
        }

        #[test]
        fn a_plugin_command_is_registered_unless_the_core_owns_the_name() {
            use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
            let base = |commands: Vec<CommandDecl>| Manifest {
                id: "registers_test_plugin".to_string(),
                version: String::new(),
                api: "0.1".to_string(),
                entry: "x.wasm".to_string(),
                tier: PluginTier::Wasm,
                name: String::new(),
                priority: 0,
                capabilities: vec![],
                subscribe: vec![],
                commands,
                requires: Vec::new(),
                tick_period: 1,
                deny_on_fault: false,
            };
            // A core name is refused (logged), not registered.
            super::register_commands(&base(vec![CommandDecl::Plain("give".to_string())]));
            assert!(crate::commands::registry::plugin_command("give").is_none());
            // A new name is registered with its op level.
            super::register_commands(&base(vec![CommandDecl::Detailed {
                name: "registers_test_cmd".to_string(),
                node: String::new(),
                op: 3,
            }]));
            let entry = crate::commands::registry::plugin_command("registers_test_cmd")
                .expect("registered");
            assert_eq!(entry.plugin, "registers_test_plugin");
            assert_eq!(entry.op, 3);
        }

        /// A minimal host for the regions event-wiring tests: KV in memory,
        /// a golden axe in hand, everyone permitted and in the overworld.
        #[derive(Default)]
        struct RegionsTestBackend {
            kv: std::sync::Mutex<std::collections::HashMap<(String, String), Vec<u8>>>,
        }

        impl aether_plugin::HostBackend for RegionsTestBackend {
            fn kv_get(&self, plugin: &str, key: &str) -> Option<Vec<u8>> {
                self.kv
                    .lock()
                    .unwrap()
                    .get(&(plugin.to_string(), key.to_string()))
                    .cloned()
            }
            fn kv_put(&self, plugin: &str, key: &str, value: &[u8]) {
                self.kv
                    .lock()
                    .unwrap()
                    .insert((plugin.to_string(), key.to_string()), value.to_vec());
            }
            fn kv_delete(&self, plugin: &str, key: &str) {
                self.kv
                    .lock()
                    .unwrap()
                    .remove(&(plugin.to_string(), key.to_string()));
            }
            fn kv_list(&self, plugin: &str, prefix: &str) -> Vec<String> {
                self.kv
                    .lock()
                    .unwrap()
                    .keys()
                    .filter(|(p, k)| p == plugin && k.starts_with(prefix))
                    .map(|(_, k)| k.clone())
                    .collect()
            }
            fn log(&self, _plugin: &str, _level: aether_plugin::LogLevel, _message: &str) {}
            fn config(&self, _plugin: &str, _key: &str) -> Option<String> {
                None
            }
            fn supports(&self, _plugin: &str, _feature: &str) -> bool {
                true
            }
            fn held_item(&self, _plugin: &str, _player: u64) -> Option<String> {
                Some("minecraft:golden_axe".to_string())
            }
            fn player_position(
                &self,
                _plugin: &str,
                _player: u64,
            ) -> Option<(String, f64, f64, f64)> {
                Some(("minecraft:overworld".to_string(), 0.0, 64.0, 0.0))
            }
            fn permission_check(&self, _plugin: &str, _id: u64, _node: &str, _op: u8) -> bool {
                true
            }
        }

        /// Load the regions plugin into a test host and install it for this
        /// thread, returning a handle whose world/registry the session-side
        /// helpers can drive.
        fn install_regions_host(
            tag: &str,
        ) -> (
            std::sync::Arc<crate::players::PlayerHandle>,
            std::sync::Arc<crate::players::Registry>,
            crate::session::DemoWorld,
            std::sync::Arc<aether_region::RegionStore>,
        ) {
            use std::sync::Arc;
            let world = test_world(9_001, tag);
            let registry: crate::players::Registry = crate::players::Registry::default();
            let registry = Arc::new(registry);
            let handle = crate::players::PlayerHandle::for_test(9_101, tag);
            registry.insert_for_test(Arc::clone(&handle));

            // A per-test store: the plugin's `Default` reads
            // `aether_region::global()`, so install this thread's own index
            // rather than replacing the process-wide one another test uses.
            let region_store = std::sync::Arc::new(aether_region::RegionStore::new());
            aether_region::install_test_store(aether_region::RegionStore::new());
            aether_region::install_test_global(std::sync::Arc::clone(&region_store));
            let backend = Arc::new(RegionsTestBackend::default());
            let mut host =
                aether_plugin::PluginHost::new(aether_plugin::HostConfig::default(), backend)
                    .unwrap();
            let registration = aether_regions_plugin::registration();
            let file = aether_plugin::ManifestFile {
                dir: std::path::PathBuf::new(),
                manifest: registration.manifest,
            };
            host.add_native(&file, &[], &Default::default(), (registration.new)())
                .expect("regions loads");
            super::register_commands(&file.manifest);
            super::install_test_host(host);
            (handle, registry, world, region_store)
        }

        #[test]
        fn a_use_block_event_reaches_the_wand_hook() {
            // Point 1 through StartDig, point 2 through UseBlock: the second
            // click is the protocol-47 tool right-click path, not a Place.
            let (handle, registry, world, store) = install_regions_host("UseBlockHook");
            assert!(
                crate::session::handle_start_dig(&handle, &registry, &world, 1, 64, 1),
                "the wand cancels the dig and resyncs the block"
            );
            crate::session::handle_use_block(&handle, &world, 4, 64, 4);
            let lines = super::on_command(
                &handle,
                "rg",
                &["define".to_string(), "useblock-hook".to_string()],
            )
            .expect("/rg is registered");
            assert!(
                lines.iter().any(|l| l.contains("Created region")),
                "{lines:?}"
            );
            assert_eq!(
                store.snapshot().len(),
                1,
                "the region went into this test's own store"
            );
        }

        #[test]
        fn a_start_dig_event_reaches_the_wand_hook() {
            // Point 2 first, then point 1 through the start-of-dig hook.
            let (handle, registry, world, store) = install_regions_host("StartDigHook");
            crate::session::handle_use_block(&handle, &world, 4, 64, 4);
            assert!(
                crate::session::handle_start_dig(&handle, &registry, &world, 1, 64, 1),
                "the wand cancels the dig and resyncs the block"
            );
            let lines = super::on_command(
                &handle,
                "rg",
                &["define".to_string(), "startdig-hook".to_string()],
            )
            .expect("/rg is registered");
            assert!(
                lines.iter().any(|l| l.contains("Created region")),
                "{lines:?}"
            );
            assert_eq!(
                store.snapshot().len(),
                1,
                "the region went into this test's own store"
            );
        }

        /// A throwaway world on a temp store. `tag` keeps every test's
        /// directory unique: tests run in parallel, and the store's `flock`
        /// refuses a second open of the same path.
        fn test_world(seed: u64, tag: &str) -> crate::session::DemoWorld {
            use aether_api::{FjallStore, World};
            let dir = std::env::temp_dir().join(format!(
                "aether-mn1-host-{}-{seed}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let store = FjallStore::open(&dir).expect("temp store");
            let generator =
                crate::worldgen::Generator::load("", "", seed).expect("noise generator");
            let revision = generator.revision();
            let cfg = crate::gencache::CacheConfig {
                root: String::new(),
                ..Default::default()
            };
            let generator = crate::gencache::Cached::with_revision(generator, seed, revision, &cfg);
            World::new(store, generator)
        }

        /// The deadlock repro from review 1 (B1), end to end through the real
        /// host: a declarative menu whose button runs a **plugin** command.
        /// Before the fix the click held the `HOST` lock and the command's
        /// dispatch re-locked it on the same thread, hanging forever; the click
        /// is run on a worker thread and joined with a timeout so a regression
        /// fails the test instead of hanging the suite.
        #[test]
        fn a_menu_command_action_runs_without_deadlocking_the_host() {
            use std::sync::Arc;
            use std::time::Duration;

            let dir = std::env::temp_dir().join(format!("aether-mn1-e2e-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let menus_dir = dir.join("menus");
            std::fs::create_dir_all(&menus_dir).unwrap();
            std::fs::write(
                menus_dir.join("shop.toml"),
                r#"
name = "shop"
title = "Shop"
layout = "chest3"
open_commands = ["shop"]

[[item]]
slot = 0
material = "minecraft:stone"
name = "Reopen"
actions = ["[command] menu shop"]
"#,
            )
            .unwrap();
            let plugins_dir = dir.join("plugins");
            std::fs::create_dir_all(&plugins_dir).unwrap();

            let mut cfg = crate::config::Config::default();
            cfg.plugins.dir = plugins_dir.to_string_lossy().into_owned();
            let mut entry = crate::config::PluginEntry::default();
            entry.config.insert(
                "menus_dir".to_string(),
                menus_dir.to_string_lossy().into_owned(),
            );
            cfg.plugins.plugins.insert("menus".to_string(), entry);
            let cfg = Arc::new(cfg);

            let world = Arc::new(test_world(31, "menus-e2e"));
            let registry: crate::players::SharedRegistry =
                Arc::new(crate::players::Registry::default());
            let handle = crate::players::PlayerHandle::for_test(9001, "Steve");
            registry.insert_for_test(Arc::clone(&handle));

            super::install(&cfg, &world, &registry);

            let (tx, rx) = std::sync::mpsc::channel();
            let world2 = Arc::clone(&world);
            let handle2 = Arc::clone(&handle);
            std::thread::spawn(move || {
                // `/shop` registers at load; open it, then click its button.
                let _ = super::on_command(&handle2, "shop", &[]);
                // Bind the id first: a `MutexGuard` in an `if let` scrutinee
                // lives for the whole body, and `click` takes the same lock.
                let id = handle2.game().window.as_ref().map(|w| w.id);
                if let Some(id) = id {
                    crate::game::window::click(&handle2, &world2, id, 0, 0, 0);
                }
                let _ = tx.send(());
            });
            rx.recv_timeout(Duration::from_secs(15))
                .expect("a `[command]` menu action deadlocked the plugin host");
            // The button's command re-opened the menu.
            assert!(handle.game().window.is_some());
        }

        #[test]
        fn a_uuid_survives_the_split_into_two_halves() {
            let uuid = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128;
            let actor = super::join_uuid(uuid as u64, (uuid >> 64) as u64);
            assert_eq!(actor.0, uuid);
        }

        #[test]
        fn a_history_window_maps_actor_area_time_and_kind() {
            let w = aether_plugin::sdk::HistoryWindow {
                has_actor: true,
                actor_low: 1,
                actor_high: 0,
                has_area: true,
                x1: -10,
                y1: 60,
                z1: -10,
                x2: 10,
                y2: 70,
                z2: 10,
                since_ms: 100,
                until_ms: 200,
                kinds: vec!["block_set".to_string()],
            };
            let mapped = super::to_window(&w).expect("known kind");
            assert_eq!(mapped.actor, Some(journal::ActorId(1)));
            assert!(mapped.area.is_some());
            assert_eq!(mapped.since_ms, 100);
            assert_eq!(mapped.until_ms, 200);
            assert_eq!(mapped.kinds, vec![1]);

            // An unknown kind selects nothing rather than widening.
            let w = aether_plugin::sdk::HistoryWindow {
                kinds: vec!["not_a_kind".to_string()],
                ..w
            };
            assert!(super::to_window(&w).is_none());
        }

        /// Load the essentials-teleport plugin over a **real** `DemoWorld` and
        /// the real `ServerBackend`, granting every capability the manifest
        /// asks for. These tests exercise `/up` end to end (host → core
        /// teleport), not a recording mock.
        fn install_essentials_host(
            tag: &str,
        ) -> (
            aether_plugin::PluginHost,
            std::sync::Arc<crate::players::PlayerHandle>,
            std::sync::Arc<crate::session::DemoWorld>,
        ) {
            use std::sync::{Arc, Mutex};
            let world = Arc::new(test_world(9_137, tag));
            let registry: crate::players::SharedRegistry =
                Arc::new(crate::players::Registry::default());
            let handle = crate::players::PlayerHandle::for_test(9_137, tag);
            registry.insert_for_test(Arc::clone(&handle));

            let registration = aether_essentials_teleport_plugin::registration();
            let caps = registration.manifest.capabilities.clone();
            let file = aether_plugin::ManifestFile {
                dir: std::path::PathBuf::new(),
                manifest: registration.manifest,
            };
            let backend = Arc::new(super::ServerBackend {
                world: Arc::clone(&world),
                config: Arc::new(crate::config::Config::default()),
                registry,
                granted: Mutex::new(std::collections::BTreeMap::from([(
                    "essentials-teleport".to_string(),
                    caps.clone(),
                )])),
                pending: Mutex::new(Vec::new()),
            });
            let mut host =
                aether_plugin::PluginHost::new(aether_plugin::HostConfig::default(), backend)
                    .expect("host");
            host.add_native(&file, &caps, &Default::default(), (registration.new)())
                .expect("essentials-teleport loads");
            // Declares each command node; the op-0 commands become Allow, so
            // the plugin's own `essentials.*` gate passes in this test.
            super::register_commands(&file.manifest);
            (host, handle, world)
        }

        fn essentials_player(handle: &crate::players::PlayerHandle) -> aether_plugin::Player {
            aether_plugin::Player {
                id: handle.entity_id as u64,
                name: handle.name.clone(),
                operator: false,
            }
        }

        /// A 3×3 patch of ordinary full-block ground at `y`, cleared above.
        fn clear_ground(world: &crate::session::DemoWorld, x: i32, y: i32, z: i32) {
            for dx in -1..=1 {
                for dz in -1..=1 {
                    assert!(world
                        .set_block(x + dx, y - 1, z + dz, "minecraft:stone")
                        .is_some());
                    for dy in 0..=4 {
                        assert!(world
                            .set_block(x + dx, y + dy, z + dz, "minecraft:air")
                            .is_some());
                    }
                }
            }
        }

        #[test]
        fn up_moves_one_block_on_ordinary_ground_through_the_real_core() {
            use crate::players::PosLook;
            let (mut host, handle, world) = install_essentials_host("UpExact");
            let (x, y, z) = (1000, 250, 1000);
            clear_ground(&world, x, y, z);
            handle.set_pos(PosLook {
                x: x as f64 + 0.5,
                y: y as f64,
                z: z as f64 + 0.5,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            });

            let player = essentials_player(&handle);
            let lines = host
                .on_command_for("essentials-teleport", &player, "up", &[])
                .expect("/up is handled")
                .lines;
            assert!(lines.iter().any(|l| l.contains("up 1")), "{lines:?}");
            // The exact path moves one block up. Before the fix the core safe
            // search snapped the air target back onto the floor at `y`.
            assert_eq!(handle.pos().y, y as f64 + 1.0, "{lines:?}");
            assert_eq!(handle.pos().x, x as f64 + 0.5);
            assert_eq!(handle.pos().z, z as f64 + 0.5);
        }

        #[test]
        fn up_stops_before_a_hazard_state_and_reports_no_room() {
            use crate::players::PosLook;
            let (mut host, handle, world) = install_essentials_host("UpLava");
            let (x, y, z) = (1001, 250, 1001);
            clear_ground(&world, x, y, z);
            // A hazard state with properties: the host must classify it by its
            // base block (`lava`), not by the full state name.
            assert!(world
                .set_block(x, y + 1, z, "minecraft:lava[level=0]")
                .is_some());
            handle.set_pos(PosLook {
                x: x as f64 + 0.5,
                y: y as f64,
                z: z as f64 + 0.5,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            });

            let player = essentials_player(&handle);
            let lines = host
                .on_command_for("essentials-teleport", &player, "up", &[])
                .expect("/up is handled")
                .lines;
            assert!(lines.iter().any(|l| l.contains("no room")), "{lines:?}");
            assert_eq!(handle.pos().y, y as f64, "the player did not move");
        }

        #[test]
        fn an_event_renders_its_places() {
            use aether_world::journal::Place;
            assert_eq!(super::place_name(&Place::Nowhere), "nowhere");
            assert_eq!(
                super::place_name(&Place::Ground { x: 1, y: 2, z: 3 }),
                "ground:1,2,3"
            );
            assert_eq!(
                super::place_name(&Place::Inventory {
                    owner: journal::ActorId(0),
                    slot: 5
                }),
                "inventory:5"
            );
        }
    }
}

#[cfg(not(feature = "plugins"))]
mod imp {
    use crate::players::{PlayerHandle, SharedRegistry};

    /// No-op: the server has no plugin host compiled in.
    pub fn install(
        _cfg: &std::sync::Arc<crate::config::Config>,
        _world: &std::sync::Arc<crate::session::DemoWorld>,
        _registry: &SharedRegistry,
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
    pub fn after_block_place(_h: &PlayerHandle, _x: i32, _y: i32, _z: i32, _b: &str) {}
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
    pub fn before_block_dig(
        _h: &PlayerHandle,
        _x: i32,
        _y: i32,
        _z: i32,
        _b: &str,
    ) -> Option<String> {
        None
    }
    pub fn before_block_interact(
        _h: &PlayerHandle,
        _x: i32,
        _y: i32,
        _z: i32,
        _b: &str,
    ) -> Option<String> {
        None
    }
    #[allow(clippy::too_many_arguments)]
    pub fn before_use_block(
        _h: &PlayerHandle,
        _x: i32,
        _y: i32,
        _z: i32,
        _i: &str,
        _b: &str,
    ) -> Option<String> {
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
    pub fn menu_click(
        _h: &PlayerHandle,
        _owner: &str,
        _menu: u64,
        _slot: i32,
        _button: i8,
        _mode: i32,
    ) {
    }
    pub fn menu_close(_h: &PlayerHandle, _owner: &str, _menu: u64) {}
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
    pub fn on_plugin_message(_h: &PlayerHandle, _c: &str, _d: &[u8]) {}
    pub fn shutdown() {}
}

pub use imp::*;
