//! The Aether **menus** plugin: one safe implementation of inventory menus,
//! as a native plugin.
//!
//! A menu is a server-invented window whose slots are stand-ins for actions,
//! not items. The core (see `aether_server::game::window`) guarantees that no
//! click on such a window can move anything into or out of a player's real
//! inventory: it intercepts every click and hands it here. This plugin owns
//! the menu state, runs the action lists the items carry, and both **exports**
//! the `menus` service (so any native or WASM dependent opens a menu with one
//! call, see [`aether_plugin::menus`]) and **loads** declarative
//! `menus/*.toml` files.
//!
//! It is a plugin like any other: the server adds it through the build-time
//! native registry, its manifest can disable it, and a dependent that
//! `requires` the `menus` service refuses to load when no provider is
//! present.

mod declarative;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use aether_plugin::manifest::{CommandDecl, Manifest, PluginTier};
use aether_plugin::menus::{
    self, MenuAction, MenuCondition, MenuDefinition, MenuItem, MenusService,
};
use aether_plugin::sdk;
use aether_plugin::service::ServiceRegistry;
use aether_plugin::{
    HostBackend, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};

/// The id under which this plugin owns every menu window it opens, so a click
/// is routed back to it and not broadcast to other native plugins.
const MENUS_OWNER: &str = "menus";

/// The menus plugin's manifest. `menu` opens a named menu; `menus` reloads
/// the declarative files (op-gated).
pub fn manifest() -> Manifest {
    Manifest {
        id: "menus".to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Aether Menus".to_string(),
        // Loads before gameplay plugins so a dependent's `requires` resolve.
        priority: 900,
        capabilities: Vec::new(),
        // No event subscription: menus are opened by commands, the API and
        // clicks, none of which is a bus event.
        // `on-command` answers `/menu`, `/menus` and each declarative menu's
        // `open_commands`; `on-tick` drives the off-thread `/menus reload`
        // (the file read runs on a worker thread and the result is installed
        // here, on the tick, never blocking it).
        subscribe: vec!["on-command".to_string(), "on-tick".to_string()],
        commands: vec![
            CommandDecl::Plain("menu".to_string()),
            CommandDecl::Detailed {
                name: "menus".to_string(),
                node: String::new(),
                op: 2,
            },
        ],
        tick_period: 1,
        deny_on_fault: false,
        tier: PluginTier::Native,
        requires: Vec::new(),
    }
}

/// The build-time registration the server adds.
pub fn registration() -> NativeRegistration {
    NativeRegistration {
        manifest: manifest(),
        new: || Box::new(MenusPlugin::default()),
    }
}

/// One open menu instance.
struct OpenMenu {
    player: u64,
    def: MenuDefinition,
}

/// The menu runtime: shared by the exported service and the click/close
/// callbacks.
#[derive(Default)]
struct Menus {
    backend: OnceLock<Arc<dyn HostBackend>>,
    services: OnceLock<Arc<ServiceRegistry>>,
    state: Mutex<State>,
    /// Declarative menus a `/menus reload` worker thread finished loading,
    /// waiting to be installed on the next tick. `None` means no reload is
    /// pending; the read itself never happens on the tick thread.
    pending: Mutex<Option<Vec<MenuDefinition>>>,
}

/// The mutable state, behind one lock.
#[derive(Default)]
struct State {
    next_id: u64,
    open: HashMap<u64, OpenMenu>,
    /// `(caller, player, menu name)` → the instance id it currently has open.
    named: HashMap<(String, u64, String), u64>,
    /// Per-player back-stack of menus, for `[back]`.
    history: HashMap<u64, Vec<(String, MenuDefinition, String)>>,
    /// Declarative menus by name.
    registry: HashMap<String, MenuDefinition>,
}

impl Menus {
    fn backend(&self) -> Option<&Arc<dyn HostBackend>> {
        self.backend.get()
    }

    fn services(&self) -> Option<&Arc<ServiceRegistry>> {
        self.services.get()
    }

    /// The items to draw: those whose slot fits and whose view condition
    /// passes for this player.
    fn visible_items(&self, player: &Player, def: &MenuDefinition) -> Vec<MenuItem> {
        let slots = def.layout.slot_count();
        def.items
            .iter()
            .filter(|i| i.slot < slots)
            .filter(|i| match &i.view_condition {
                Some(c) => self.condition_ok(player, c),
                None => true,
            })
            .cloned()
            .collect()
    }

    /// Open `def` for `player`, replacing whatever they had open and pushing
    /// the replaced menu onto the per-player history so `[back]` can return.
    ///
    /// A player has exactly one window, so this drops the previous instance
    /// (of any name) before installing the new one — a `[open]` chain never
    /// leaks a stale instance. The window is owned by [`MENUS_OWNER`], not by
    /// the dependent that asked for it: *this* plugin runs the actions, so it
    /// must be the plugin a click is routed back to.
    fn open_internal(
        &self,
        caller: &str,
        player: &Player,
        def: &MenuDefinition,
    ) -> Result<(), String> {
        if def.layout.slot_count() == 0 {
            return Err("menu layout has no slots".to_string());
        }
        let items = self.visible_items(player, def);
        let id = {
            let mut st = self.state.lock().unwrap();
            // The menu this player currently has open, if any.
            let prev = st
                .open
                .values()
                .find(|m| m.player == player.id)
                .map(|m| m.def.clone());
            // A player has one window: drop every instance of theirs.
            st.open.retain(|_, m| m.player != player.id);
            st.named.retain(|(_, p, _), _| *p != player.id);
            // Remember the replaced menu for `[back]`, unless it is the same
            // one (reopening a menu must not push a self-loop).
            if let Some(prev) = prev {
                if prev.name != def.name {
                    st.history.entry(player.id).or_default().push((
                        caller.to_string(),
                        prev,
                        caller.to_string(),
                    ));
                }
            }
            st.next_id += 1;
            let id = st.next_id;
            st.open.insert(
                id,
                OpenMenu {
                    player: player.id,
                    def: def.clone(),
                },
            );
            st.named
                .insert((caller.to_string(), player.id, def.name.clone()), id);
            id
        };
        let Some(backend) = self.backend() else {
            return Err("menus: host backend unavailable".to_string());
        };
        let title = if def.title.is_empty() {
            def.name.clone()
        } else {
            def.title.clone()
        };
        if backend.open_menu(MENUS_OWNER, player.id, id, &title, def.layout, &items) {
            Ok(())
        } else {
            self.handle_close(player, id);
            Err("menus: the host cannot open a window for that player".to_string())
        }
    }

    /// The instance id currently named `(caller, player, name)`, if any.
    fn current_id(&self, st: &State, caller: &str, player: u64, name: &str) -> Option<u64> {
        st.named
            .get(&(caller.to_string(), player, name.to_string()))
            .copied()
    }

    /// A click reached a menu. Returns the actions run, for tests.
    fn handle_click(&self, player: &Player, menu: u64, slot: i32, _button: i8, _mode: i32) {
        // A click outside the window (`-999`) or past the menu's own slots
        // (the player's inventory rows) runs nothing. The core has already
        // made sure it changed no item.
        let Some(clicked) = self.click_item(player, menu, slot) else {
            return;
        };
        if let Some(cond) = &clicked.click_condition {
            if !self.condition_ok(player, cond) {
                return;
            }
        }
        self.run_actions(player, menu, &clicked.actions);
    }

    /// The item at `slot` if the click is in this player's menu.
    fn click_item(&self, player: &Player, menu: u64, slot: i32) -> Option<MenuItem> {
        let Ok(slot) = usize::try_from(slot) else {
            return None;
        };
        let st = self.state.lock().unwrap();
        let open = st.open.get(&menu)?;
        if open.player != player.id {
            return None;
        }
        if slot >= open.def.layout.slot_count() as usize {
            return None;
        }
        open.def
            .items
            .iter()
            .find(|i| i.slot as usize == slot)
            .cloned()
    }

    fn handle_close(&self, player: &Player, menu: u64) {
        let mut st = self.state.lock().unwrap();
        if let Some(open) = st.open.get(&menu) {
            if open.player == player.id {
                st.open.remove(&menu);
            }
        }
        st.named.retain(|_, id| *id != menu);
    }

    fn run_actions(&self, player: &Player, menu: u64, actions: &[MenuAction]) {
        for action in actions {
            match action {
                MenuAction::Command(cmd) => {
                    let cmd = self.expand(cmd, player);
                    self.run_command(player, Some(player.id), &cmd);
                }
                MenuAction::Console(cmd) => {
                    let cmd = self.expand(cmd, player);
                    self.run_command(player, None, &cmd);
                }
                MenuAction::Message(text) => {
                    let text = self.expand(text, player);
                    if let Some(b) = self.backend() {
                        b.send_message("menus", player.id, &text);
                    }
                }
                MenuAction::Open(name) => {
                    let def = self.state.lock().unwrap().registry.get(name).cloned();
                    match def {
                        Some(def) => {
                            if let Err(e) = self.open_internal("menus", player, &def) {
                                self.log(&format!("open `{name}`: {e}"));
                            }
                        }
                        None => self.log(&format!("open: no menu named `{name}`")),
                    }
                }
                MenuAction::Close => {
                    if let Some(b) = self.backend() {
                        b.close_menu("menus", player.id, menu);
                    }
                    self.handle_close(player, menu);
                }
                MenuAction::Sound(name) => {
                    let name = self.expand(name, player);
                    if let Some(b) = self.backend() {
                        b.play_sound("menus", player.id, &name);
                    }
                }
                MenuAction::EconomyTake(_amount) => {
                    // Economy is an optional service; with none registered the
                    // action is refused (never charged twice, never free).
                    self.log("economy action ignored: no economy service");
                }
                MenuAction::Give { item, count } => {
                    // Give through the core, so the overflow is dropped at the
                    // player's feet and journalled, exactly like `/give`. (The
                    // SDK `players.give` is a stub until CMD-32 lands.)
                    let item = self.expand(item, player);
                    if let Some(b) = self.backend() {
                        let left = b.give_item("menus", player.id, &item, *count);
                        if left > 0 {
                            self.log(&format!(
                                "give `{item}` x{left} could not be handed over; left is {left}"
                            ));
                        }
                    }
                }
                MenuAction::Back => self.go_back(player),
            }
        }
    }

    fn run_command(&self, player: &Player, as_player: Option<u64>, cmd: &str) {
        let Some(b) = self.backend() else { return };
        for line in b.run_command("menus", as_player, cmd) {
            b.send_message("menus", player.id, &line);
        }
    }

    fn go_back(&self, player: &Player) {
        let prev = {
            let mut st = self.state.lock().unwrap();
            let Some(stack) = st.history.get_mut(&player.id) else {
                return;
            };
            stack.pop()
        };
        let Some((caller, def, _)) = prev else {
            return;
        };
        if let Err(e) = self.open_internal(&caller, player, &def) {
            self.log(&format!("back: {e}"));
        }
    }

    /// Whether `cond` holds for `player`.
    fn condition_ok(&self, player: &Player, cond: &MenuCondition) -> bool {
        match cond {
            MenuCondition::Permission(node) => self
                .services()
                .and_then(|s| sdk::permissions(s))
                .map(|p| p.check("menus", player, node))
                .unwrap_or(false),
            MenuCondition::InGroup(group) => self
                .services()
                .and_then(|s| sdk::permissions(s))
                .map(|p| p.in_group("menus", player, group))
                .unwrap_or(false),
            // No economy service is registered on this branch; a balance
            // condition fails closed rather than showing an item for free.
            MenuCondition::BalanceAtLeast(_) => false,
            MenuCondition::Equals { placeholder, value } => {
                &self.expand(placeholder, player) == value
            }
            MenuCondition::AtLeast { placeholder, value } => self
                .expand(placeholder, player)
                .trim()
                .parse::<i64>()
                .map(|n| n >= *value)
                .unwrap_or(false),
        }
    }

    /// Expand the placeholders a menu may use.
    ///
    /// `%player%` is built in and always expanded here. Anything else is left
    /// verbatim: an unknown placeholder must not silently vanish.
    ///
    /// **Hook for the shared placeholders service (SB1).** SB1 moves a
    /// `placeholders` service into the SDK plugin; that service is the single
    /// place plugin-provided placeholders should resolve, and this method is
    /// where it plugs in — after the built-in substitutions, for each `%name%`
    /// the text still contains. It is deliberately **not** implemented yet, to
    /// avoid a second, divergent placeholder engine; when SB1 lands, resolve
    /// through `self.services()` and leave a name it does not know verbatim.
    fn expand(&self, text: &str, player: &Player) -> String {
        // --- built-in placeholders -----------------------------------------
        let text = text.replace("%player%", &player.name);
        // --- hook: shared `placeholders` service (SB1) ---------------------
        // TODO(SB1): `placeholder(services, text, player)` when the service
        // exists. Do not grow a local engine here.
        text
    }

    fn log(&self, message: &str) {
        if let Some(b) = self.backend() {
            b.log("menus", LogLevel::Warn, message);
        }
    }

    /// The named declarative menu, if any.
    fn named_menu(&self, name: &str) -> Option<MenuDefinition> {
        self.state.lock().unwrap().registry.get(name).cloned()
    }

    /// The menu an `open_commands` entry opens, if any.
    fn menu_for_command(&self, command: &str) -> Option<MenuDefinition> {
        self.state
            .lock()
            .unwrap()
            .registry
            .values()
            .find(|d| d.open_commands.iter().any(|c| c == command))
            .cloned()
    }

    /// Every `(command, menu)` pair a declarative file asked for, for the
    /// plugin to register with the core command registry.
    fn open_command_bindings(&self) -> Vec<(String, MenuDefinition)> {
        let mut out = Vec::new();
        for def in self.state.lock().unwrap().registry.values() {
            for cmd in &def.open_commands {
                out.push((cmd.clone(), def.clone()));
            }
        }
        out
    }

    /// Insert/replace the declarative registry, keeping any menu a live
    /// instance is currently showing.
    fn install_menus(&self, menus: Vec<MenuDefinition>) {
        let mut st = self.state.lock().unwrap();
        st.registry.clear();
        for def in menus {
            st.registry.insert(def.name.clone(), def);
        }
    }

    /// Take the menus a reload worker finished reading, if any. Called once
    /// per tick by the plugin.
    fn take_pending(&self) -> Option<Vec<MenuDefinition>> {
        self.pending.lock().unwrap().take()
    }
}

impl MenusService for Menus {
    fn open(&self, caller: &str, player: &Player, def: &MenuDefinition) -> Result<(), String> {
        // `open_internal` records the replaced menu for `[back]` itself.
        self.open_internal(caller, player, def)
    }

    fn update(
        &self,
        caller: &str,
        player: &Player,
        name: &str,
        slot: u32,
        item: Option<MenuItem>,
    ) -> Result<(), String> {
        let id = {
            let st = self.state.lock().unwrap();
            self.current_id(&st, caller, player.id, name)
        };
        let Some(id) = id else {
            return Err(format!("no open menu `{name}` for this caller"));
        };
        // The stored definition is what a later click reads, so update it too.
        {
            let mut st = self.state.lock().unwrap();
            if let Some(open) = st.open.get_mut(&id) {
                if let Some(it) = &item {
                    if let Some(existing) = open.def.items.iter_mut().find(|i| i.slot == slot) {
                        *existing = it.clone();
                    } else {
                        open.def.items.push(it.clone());
                    }
                } else {
                    open.def.items.retain(|i| i.slot != slot);
                }
            }
        }
        let Some(backend) = self.backend() else {
            return Err("menus: host backend unavailable".to_string());
        };
        let item2 = item.unwrap_or_else(|| MenuItem {
            slot,
            item: String::new(),
            count: 0,
            name: None,
            lore: Vec::new(),
            glint: false,
            model: None,
            view_condition: None,
            click_condition: None,
            actions: Vec::new(),
        });
        backend.set_menu_slot("menus", player.id, id, slot, Some(&item2));
        Ok(())
    }

    fn close(&self, caller: &str, player: &Player, name: &str) -> Result<(), String> {
        let id = {
            let st = self.state.lock().unwrap();
            self.current_id(&st, caller, player.id, name)
        };
        let Some(id) = id else {
            return Err(format!("no open menu `{name}` for this caller"));
        };
        if let Some(b) = self.backend() {
            b.close_menu("menus", player.id, id);
        }
        self.handle_close(player, id);
        Ok(())
    }
}

/// The native plugin wrapper: `init` publishes the service and loads the
/// declarative files; the callbacks drive the runtime.
pub struct MenusPlugin {
    inner: Arc<Menus>,
}

impl Default for MenusPlugin {
    fn default() -> Self {
        MenusPlugin {
            inner: Arc::new(Menus::default()),
        }
    }
}

impl MenusPlugin {
    /// Register a declarative menu's `open_commands` with the core command
    /// registry, so `/shop` is dispatched and permission-checked like any
    /// other command. A core name or a duplicate is refused (logged) by the
    /// registry, never shadowed.
    fn register_open_commands(&self, ctx: &mut NativeContext) {
        for (cmd, def) in self.inner.open_command_bindings() {
            let node = def
                .permission
                .clone()
                .unwrap_or_else(|| format!("plugin.menus.menu.{}", def.name));
            if let Err(e) = ctx.backend.register_command("menus", &cmd, &node, 0) {
                ctx.log(
                    LogLevel::Warn,
                    &format!("menu `{}`: command `{cmd}` not registered: {e}", def.name),
                );
            }
        }
    }
}

impl NativePlugin for MenusPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let _ = self.inner.backend.set(Arc::clone(ctx.backend));
        let _ = self.inner.services.set(Arc::clone(ctx.services));
        // Load declarative menus before publishing, so an early command finds
        // them.
        let loaded = declarative::load(ctx);
        let names: Vec<String> = loaded.iter().map(|d| d.name.clone()).collect();
        self.inner.install_menus(loaded);
        self.register_open_commands(ctx);
        // Register the exported service.
        menus::register_menus(ctx.services, self.inner.clone())?;
        ctx.log(
            LogLevel::Info,
            &format!(
                "menus service registered; {} declarative menu(s): {}",
                names.len(),
                names.join(", ")
            ),
        );
        Ok(())
    }

    fn on_command(
        &mut self,
        _ctx: &mut NativeContext,
        p: &Player,
        name: &str,
        args: &[String],
    ) -> Option<aether_plugin::bindings::aether::plugin::types::CommandResult> {
        let def = match name {
            "menu" => {
                let Some(menu_name) = args.first() else {
                    return Some(lines(vec!["Usage: /menu <name>".to_string()]));
                };
                match self.inner.named_menu(menu_name) {
                    Some(def) => def,
                    None => {
                        return Some(lines(vec![format!("No menu named `{menu_name}`.")]));
                    }
                }
            }
            "menus" if args.first().map(String::as_str) == Some("reload") => {
                // Read the files on a worker thread and install them on the
                // next tick: the tick thread must not block on filesystem I/O.
                let dir = declarative::dir_from_config(_ctx.config);
                let backend = Arc::clone(_ctx.backend);
                let inner = Arc::clone(&self.inner);
                let spawned = std::thread::Builder::new()
                    .name("menus-reload".into())
                    .spawn(move || {
                        let loaded = declarative::load_dir(&dir, &backend);
                        *inner.pending.lock().unwrap() = Some(loaded);
                    });
                return Some(match spawned {
                    Ok(_) => lines(vec!["Reloading menus…".to_string()]),
                    Err(e) => lines(vec![format!("menus: cannot start reload: {e}")]),
                });
            }
            "menus" => return Some(lines(vec!["Usage: /menus reload".to_string()])),
            // A declarative menu's own `open_commands` entry.
            _ => self.inner.menu_for_command(name)?,
        };
        // Permission is checked before the window opens.
        if let Some(node) = &def.permission {
            let allowed = self
                .inner
                .services()
                .and_then(|s| sdk::permissions(s))
                .map(|svc| svc.check("menus", p, node))
                .unwrap_or(false);
            if !allowed {
                return Some(lines(vec!["You may not open that menu.".to_string()]));
            }
        }
        Some(match self.inner.open_internal("menus", p, &def) {
            Ok(()) => lines(Vec::new()),
            Err(e) => lines(vec![e]),
        })
    }

    fn on_menu_click(
        &mut self,
        _ctx: &mut NativeContext,
        p: &Player,
        menu: u64,
        slot: i32,
        button: i8,
        mode: i32,
    ) {
        self.inner.handle_click(p, menu, slot, button, mode);
    }

    fn on_menu_close(&mut self, _ctx: &mut NativeContext, p: &Player, menu: u64) {
        self.inner.handle_close(p, menu);
    }

    fn on_tick(&mut self, ctx: &mut NativeContext, _n: u64) {
        // Install any declarative menus a reload worker finished reading. This
        // is the only place the registry changes after `init`, so the read
        // never blocks the tick.
        if let Some(menus) = self.inner.take_pending() {
            let n = menus.len();
            self.inner.install_menus(menus);
            self.register_open_commands(ctx);
            ctx.log(LogLevel::Info, &format!("Reloaded {n} menu(s)."));
        }
    }
}

fn lines(v: Vec<String>) -> aether_plugin::bindings::aether::plugin::types::CommandResult {
    aether_plugin::bindings::aether::plugin::types::CommandResult {
        handled: true,
        lines: v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_plugin::menus::{MenuBuilder, MenuLayout as L};
    use std::sync::Mutex as StdMutex;

    /// A backend that records menu calls and the "real inventory" a click must
    /// never touch.
    #[derive(Default)]
    struct Rec {
        opened: StdMutex<Vec<(u64, String)>>,
        closed: StdMutex<Vec<u64>>,
        commands: StdMutex<Vec<String>>,
        messages: StdMutex<Vec<String>>,
        sounds: StdMutex<Vec<String>>,
        /// `(item, count)` pairs handed over through `give_item`.
        given: StdMutex<Vec<(String, u32)>>,
    }

    impl HostBackend for Rec {
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
        fn open_menu(
            &self,
            _plugin: &str,
            _player: u64,
            menu: u64,
            title: &str,
            _layout: aether_plugin::menus::MenuLayout,
            _items: &[MenuItem],
        ) -> bool {
            self.opened.lock().unwrap().push((menu, title.to_string()));
            true
        }
        fn close_menu(&self, _: &str, _: u64, menu: u64) {
            self.closed.lock().unwrap().push(menu);
        }
        fn send_message(&self, _: &str, _: u64, text: &str) {
            self.messages.lock().unwrap().push(text.to_string());
        }
        fn play_sound(&self, _: &str, _: u64, name: &str) {
            self.sounds.lock().unwrap().push(name.to_string());
        }
        fn give_item(&self, _: &str, _: u64, item: &str, count: u32) -> u64 {
            self.given.lock().unwrap().push((item.to_string(), count));
            0
        }
        fn run_command(&self, _: &str, _: Option<u64>, cmd: &str) -> Vec<String> {
            self.commands.lock().unwrap().push(cmd.to_string());
            Vec::new()
        }
    }

    fn player() -> Player {
        Player {
            id: 7,
            name: "Steve".to_string(),
            operator: false,
        }
    }

    fn menus_with(backend: Arc<Rec>) -> Menus {
        let m = Menus::default();
        let _ = m.backend.set(backend);
        m
    }

    fn shop() -> MenuDefinition {
        MenuBuilder::new("shop")
            .title("&6Shop")
            .layout(L::Chest3)
            .button(
                0,
                "minecraft:emerald",
                "Buy",
                vec![MenuAction::Command("buy sword".into())],
            )
            .button(1, "minecraft:barrier", "Close", vec![MenuAction::Close])
            .build()
    }

    #[test]
    fn opening_a_menu_calls_the_host_once() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        m.open("caller", &player(), &shop()).unwrap();
        assert_eq!(rec.opened.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_click_runs_the_slots_actions_for_every_click_type() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        m.open("caller", &player(), &shop()).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;

        // A left-click on the buy button runs its command.
        m.handle_click(&player(), id, 0, 0, 0);
        assert_eq!(rec.commands.lock().unwrap().as_slice(), ["buy sword"]);
        // Shift-click, right-click, number-key swap, drop, drag and creative
        // middle-click on the same slot all run the same action: the click
        // *type* decides nothing here.
        for (button, mode) in [(0, 1), (1, 0), (2, 2), (0, 4), (0, 5), (2, 3)] {
            m.handle_click(&player(), id, 0, button, mode);
        }
        assert!(rec
            .commands
            .lock()
            .unwrap()
            .iter()
            .all(|c| c == "buy sword"));
    }

    #[test]
    fn a_give_action_hands_items_over_through_the_host() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        let def = MenuBuilder::new("g")
            .button(
                0,
                "minecraft:chest",
                "Take",
                vec![MenuAction::Give {
                    item: "minecraft:diamond".into(),
                    count: 8,
                }],
            )
            .build();
        m.open("caller", &player(), &def).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;
        m.handle_click(&player(), id, 0, 0, 0);
        assert_eq!(
            rec.given.lock().unwrap().as_slice(),
            [("minecraft:diamond".to_string(), 8)]
        );
    }

    #[test]
    fn back_returns_to_the_previous_menu() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        let first = MenuBuilder::new("first")
            .button(
                0,
                "minecraft:compass",
                "Go",
                vec![MenuAction::Open("second".into())],
            )
            .build();
        let second = MenuBuilder::new("second")
            .button(0, "minecraft:oak_door", "Back", vec![MenuAction::Back])
            .build();
        m.install_menus(vec![first, second]);
        let first = m.named_menu("first").unwrap();

        m.open("menus", &player(), &first).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;
        // Open `second` from `first`; the click also pushes `first`.
        m.handle_click(&player(), id, 0, 0, 0);
        let id2 = rec.opened.lock().unwrap()[1].0;
        // `back` reopens `first` (its title falls back to its name).
        m.handle_click(&player(), id2, 0, 0, 0);
        let opened = rec.opened.lock().unwrap();
        assert_eq!(opened.len(), 3);
        assert_eq!(opened.last().unwrap().1, "first");
    }

    #[test]
    fn a_click_on_a_player_inventory_row_runs_nothing() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        m.open("caller", &player(), &shop()).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;
        // Slot 27 is the first player row for a 3-row chest: no menu item.
        m.handle_click(&player(), id, 27, 0, 0);
        assert!(rec.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn a_close_action_closes_the_window() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        m.open("caller", &player(), &shop()).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;
        m.handle_click(&player(), id, 1, 0, 0);
        assert_eq!(rec.closed.lock().unwrap().as_slice(), [id]);
    }

    #[test]
    fn a_late_click_after_close_is_ignored() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        m.open("caller", &player(), &shop()).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;
        m.handle_close(&player(), id);
        // A click that races the close finds no menu and runs nothing.
        m.handle_click(&player(), id, 0, 0, 0);
        assert!(rec.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn a_click_on_another_players_menu_is_ignored() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        m.open("caller", &player(), &shop()).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;
        let other = Player {
            id: 8,
            name: "Alex".to_string(),
            operator: false,
        };
        m.handle_click(&other, id, 0, 0, 0);
        assert!(rec.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn update_replaces_the_stored_item_the_next_click_reads() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        m.open("caller", &player(), &shop()).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;
        let mut item = MenuItem::filler("minecraft:diamond", "Now diamond");
        item.slot = 0;
        item.actions.push(MenuAction::Message("hi".into()));
        m.update("caller", &player(), "shop", 0, Some(item))
            .unwrap();
        m.handle_click(&player(), id, 0, 0, 0);
        assert_eq!(rec.messages.lock().unwrap().as_slice(), ["hi"]);
    }

    #[test]
    fn a_sound_action_plays_through_the_host() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        let def = MenuBuilder::new("s")
            .button(
                0,
                "minecraft:bell",
                "Ding",
                vec![MenuAction::Sound("minecraft:block.note_block.bell".into())],
            )
            .build();
        m.open("caller", &player(), &def).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;
        m.handle_click(&player(), id, 0, 0, 0);
        assert_eq!(
            rec.sounds.lock().unwrap().as_slice(),
            ["minecraft:block.note_block.bell"]
        );
    }

    #[test]
    fn placeholders_expand_the_player_name() {
        let m = Menus::default();
        assert_eq!(m.expand("Hi %player%!", &player()), "Hi Steve!");
        assert_eq!(m.expand("%unknown%", &player()), "%unknown%");
    }

    #[test]
    fn random_click_sequences_run_only_the_menus_own_actions() {
        // The menus plugin has no inventory method to call at all — the slots
        // are virtual and the core guarantees the inventory is untouched (the
        // real end-to-end property test lives in `game::window`). What this
        // bounds here is that a click can only ever produce an effect the menu
        // authored: no click type or slot invents a command, message or gift.
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        let mut builder = MenuBuilder::new("all").layout(L::Chest6);
        for slot in 0..54 {
            builder = builder.button(
                slot,
                "minecraft:stone",
                "Button",
                vec![
                    MenuAction::Command("say hi".into()),
                    MenuAction::Sound("minecraft:ui.button.click".into()),
                ],
            );
        }
        m.open("caller", &player(), &builder.build()).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;

        // Every click type over random slots, including the player rows and
        // outside the window.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..50_000 {
            let slot = ((next() % 130) as i64 - 30) as i32;
            let button = (next() % 12) as i8;
            let mode = (next() % 8) as i32;
            m.handle_click(&player(), id, slot, button, mode);
        }
        assert!(rec.commands.lock().unwrap().iter().all(|c| c == "say hi"));
        assert!(rec
            .sounds
            .lock()
            .unwrap()
            .iter()
            .all(|s| s == "minecraft:ui.button.click"));
        assert!(rec.messages.lock().unwrap().is_empty());
        assert!(rec.given.lock().unwrap().is_empty());
    }

    #[test]
    fn reload_reads_off_thread_and_installs_on_the_next_tick() {
        use aether_plugin::service::ServiceRegistry;
        use std::collections::BTreeMap;

        let dir = std::env::temp_dir().join(format!("aether-menus-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.toml"),
            "name = \"a\"\n[[item]]\nslot = 0\nmaterial = \"stone\"\n",
        )
        .unwrap();

        let backend: Arc<dyn HostBackend> = Arc::new(Rec::default());
        let services = Arc::new(ServiceRegistry::new());
        let mut config = BTreeMap::new();
        config.insert("menus_dir".to_string(), dir.to_string_lossy().into_owned());
        let caps: Vec<String> = Vec::new();
        let mut ctx = NativeContext {
            id: "menus",
            capabilities: &caps,
            config: &config,
            services: &services,
            backend: &backend,
        };

        let mut plugin = MenusPlugin::default();
        plugin.init(&mut ctx).unwrap();
        assert!(plugin.inner.named_menu("a").is_some());
        assert!(plugin.inner.named_menu("b").is_none());

        // A second file appears; `/menus reload` must not read it on this
        // (tick) thread — it returns at once and the read happens off-thread.
        std::fs::write(
            dir.join("b.toml"),
            "name = \"b\"\n[[item]]\nslot = 0\nmaterial = \"dirt\"\n",
        )
        .unwrap();
        plugin.on_command(&mut ctx, &player(), "menus", &["reload".to_string()]);

        // The new menu is only present once a tick installs the worker's
        // result.
        let mut installed = false;
        for _ in 0..2000 {
            plugin.on_tick(&mut ctx, 1);
            if plugin.inner.named_menu("b").is_some() {
                installed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(installed, "the reload result was never installed on a tick");
    }

    #[test]
    fn a_declarative_menu_loads_opens_and_acts() {
        let rec = Arc::new(Rec::default());
        let m = menus_with(rec.clone());
        let def = declarative::parse_str(
            r#"
name = "shop"
title = "&6Shop"
layout = "chest6"
open_commands = ["shop"]
permission = "shop.use"

[[item]]
slot = 0
material = "emerald"
name = "&aBuy"
actions = ["[message] &aBought!", "[close]"]
"#,
        )
        .unwrap();
        m.install_menus(vec![def]);

        // The open command resolves to the menu.
        let def = m.menu_for_command("shop").expect("bound command");
        assert_eq!(def.layout, aether_plugin::menus::MenuLayout::Chest6);
        m.open("menus", &player(), &def).unwrap();
        let id = rec.opened.lock().unwrap()[0].0;

        m.handle_click(&player(), id, 0, 0, 0);
        assert_eq!(rec.messages.lock().unwrap().as_slice(), ["&aBought!"]);
        assert_eq!(rec.closed.lock().unwrap().as_slice(), [id]);
    }
}
