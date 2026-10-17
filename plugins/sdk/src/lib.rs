//! The Aether plugin SDK, as a **native plugin**.
//!
//! This is the runtime half of the SDK: it runs in-process and publishes its
//! helpers as services (`sdk.permissions@0.1`, `sdk.commands@0.1`,
//! `sdk.messages@0.1`, `sdk.players@0.1`, …) through the host's service
//! registry. A dependent plugin — native or WASM — declares the dependency and
//! calls these with one function call; the host forwards a WASM caller's
//! import to [`SdkPlugin`] in-process.
//!
//! The SDK is deliberately **thin**: it holds state the helpers genuinely need
//! (the command registry, the message tables) and otherwise fronts core
//! mechanisms through the [`HostBackend`] the host hands it. It never
//! implements the game itself. Permission **reads** front the core store
//! (CMD-01) through the backend and fail closed when there is none; the
//! permission **write** model (CMD-02) and teleport/give (CMD-31/32) are not
//! on this branch, so those calls return the neutral answer rather than
//! guessing. That is the approved design: facades, not a second
//! implementation.
//!
//! It is a plugin like any other: the server adds it through the build-time
//! native registry, its manifest can disable it, and a dependent that
//! `requires` one of its services refuses to load when no provider is present.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use aether_plugin::manifest::{Manifest, PluginTier};
use aether_plugin::placeholders::{
    register_placeholders, PlaceholderFn, PlaceholderQuery, PlaceholdersService,
};
use aether_plugin::sdk::{
    self, ArgKind, CommandSpec, CommandsService, ConfirmService, CooldownsService, HistoryPage,
    HistoryService, HistoryWindow, MessagesService, PermissionsService, PlayersService, Pos,
    Resolution, SchedulerService, TeleportCause, TitleTimes,
};
use aether_plugin::{
    HostBackend, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};

/// The SDK plugin's manifest. It subscribes to `on-tick` to advance the timer
/// wheel (`scheduler`) and expire cooldowns/confirmations; otherwise it is a
/// service host driven by the calls its dependents make.
pub fn manifest() -> Manifest {
    Manifest {
        id: "sdk".to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Aether SDK".to_string(),
        // Loads before gameplay plugins; the value only orders event calls.
        priority: 1000,
        capabilities: Vec::new(),
        subscribe: vec!["on-tick".to_string()],
        commands: Vec::new(),
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
        new: || Box::new(SdkPlugin::default()),
    }
}

/// Capability names the SDK service layer gates on.
///
/// A dependent asks for these in `plugin.toml` and the operator grants them;
/// the host records the effective grant and the gate reads it by caller id.
/// Checking here, in the service implementation, is the single gate — it
/// applies to native dependents and WASM ones alike, and the host bridge does
/// not duplicate it.
const CAP_PERMISSIONS_ADMIN: &str = "permissions.admin";
const CAP_MESSAGES_SEND: &str = "messages.send";
const CAP_MESSAGES_BROADCAST: &str = "messages.broadcast";
/// Read access to the world's history (PA2). A plugin asks for it in
/// `plugin.toml`; the operator grants it; the gate reads the caller's
/// effective grant by id.
const CAP_HISTORY_READ: &str = "history.read";

/// The SDK service host.
pub struct SdkPlugin {
    commands: Arc<Commands>,
    messages: Arc<Messages>,
    players: Arc<Players>,
    scheduler: Arc<Scheduler>,
    cooldowns: Arc<Cooldowns>,
    confirm: Arc<Confirm>,
    placeholders: Arc<Placeholders>,
}

impl Default for SdkPlugin {
    fn default() -> Self {
        // One shared slot the services read the host backend from, filled in
        // `init` before any dependent can call.
        let backend: Arc<OnceLock<Arc<dyn HostBackend>>> = Arc::new(OnceLock::new());
        // One shared tick clock the timer-driven services advance from.
        let clock = Arc::new(Clock::default());
        let messages = Arc::new(Messages::new(Arc::clone(&backend)));
        SdkPlugin {
            commands: Arc::new(Commands::new(Arc::clone(&backend))),
            scheduler: Arc::new(Scheduler::new(Arc::clone(&clock))),
            cooldowns: Arc::new(Cooldowns::new(Arc::clone(&clock))),
            confirm: Arc::new(Confirm::new(clock, Arc::clone(&messages))),
            messages,
            players: Arc::new(Players::new(Arc::clone(&backend))),
            placeholders: Arc::new(Placeholders::new(backend)),
        }
    }
}

impl NativePlugin for SdkPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let backend = Arc::clone(ctx.backend);
        self.commands.set_backend(Arc::clone(&backend));
        self.messages.set_backend(Arc::clone(&backend));
        self.placeholders.set_backend(Arc::clone(&backend));
        self.players.set_backend(backend);
        // The resolved-token cache is a tick count; let the operator widen or
        // disable it (`[plugins.sdk] placeholder_cache_ticks`).
        if let Some(ttl) = ctx
            .config
            .get("placeholder_cache_ticks")
            .and_then(|v| v.parse().ok())
        {
            self.placeholders.set_ttl(ttl);
        }

        let commands: Arc<dyn CommandsService> = self.commands.clone();
        sdk::register_commands(ctx.services, commands)?;
        // The permission service gates its admin writes itself, reading the
        // caller's effective capabilities from the host backend — the single
        // gate, applied to native and WASM callers alike.
        let permissions: Arc<dyn PermissionsService> =
            Arc::new(Permissions::new(Arc::clone(ctx.backend)));
        sdk::register_permissions(ctx.services, permissions)?;
        let messages: Arc<dyn MessagesService> = self.messages.clone();
        sdk::register_messages(ctx.services, messages)?;
        let players: Arc<dyn PlayersService> = self.players.clone();
        sdk::register_players(ctx.services, players)?;
        let scheduler: Arc<dyn SchedulerService> = self.scheduler.clone();
        sdk::register_scheduler(ctx.services, scheduler)?;
        let cooldowns: Arc<dyn CooldownsService> = self.cooldowns.clone();
        sdk::register_cooldowns(ctx.services, cooldowns)?;
        let confirm: Arc<dyn ConfirmService> = self.confirm.clone();
        sdk::register_confirm(ctx.services, confirm)?;
        // History is a facade over the host's journal: every query is
        // capability-gated on the caller's `history.read` grant here, so native
        // and WASM dependents are checked alike.
        let history: Arc<dyn HistoryService> = Arc::new(History::new(Arc::clone(ctx.backend)));
        sdk::register_history(ctx.services, history)?;
        let placeholders: Arc<dyn PlaceholdersService> = self.placeholders.clone();
        register_placeholders(ctx.services, placeholders)?;

        ctx.log(
            LogLevel::Info,
            "SDK services registered (permissions, commands, messages, players, \
             scheduler, cooldowns, confirm, history, placeholders)",
        );
        Ok(())
    }

    fn on_tick(&mut self, _ctx: &mut NativeContext, n: u64) {
        // Advance the clock the timer-driven services read, then fire due
        // timers into the pending queue a dependent polls.
        self.scheduler.advance(n);
        // Refresh the placeholder query memo for the new tick.
        self.placeholders.advance(n);
    }
}

// --- commands ----------------------------------------------------------

/// The command registry the `sdk.commands` service fronts.
///
/// Keyed by `(caller, name)` so two dependents may each register their own
/// commands without colliding, and so a caller can only parse its own specs.
///
/// A **root** spec (no `parent`) is also registered in the core command
/// registry (via the host backend), so it is dispatched and permission-checked
/// by the core's single gate — a command reaching the SDK is never a side path
/// around it. A **subcommand** (`parent` set) is stored only here, under its
/// parent, and is reached by `resolve` when the first argument matches — it is
/// never a top-level command.
#[derive(Default)]
struct Commands {
    /// Root specs: caller id → command name → spec.
    roots: Mutex<HashMap<String, HashMap<String, CommandSpec>>>,
    /// Subcommands: caller id → parent name → subcommand name → spec.
    subs: Mutex<HashMap<String, HashMap<String, HashMap<String, CommandSpec>>>>,
    backend: Arc<OnceLock<Arc<dyn HostBackend>>>,
}

impl Commands {
    fn new(backend: Arc<OnceLock<Arc<dyn HostBackend>>>) -> Commands {
        Commands {
            roots: Mutex::new(HashMap::new()),
            subs: Mutex::new(HashMap::new()),
            backend,
        }
    }

    fn set_backend(&self, backend: Arc<dyn HostBackend>) {
        let _ = self.backend.set(backend);
    }

    fn register(&self, caller: &str, spec: CommandSpec) -> Result<(), String> {
        if spec.name.is_empty() {
            return Err("command name must not be empty".to_string());
        }
        // A subcommand is stored under its parent and never registered at the
        // top level: the core routes `parent` to the plugin, and `resolve`
        // picks the subcommand out of the arguments.
        if let Some(parent) = spec.parent.clone().filter(|p| !p.is_empty()) {
            let mut subs = self.subs.lock().unwrap();
            let by_name = subs
                .entry(caller.to_string())
                .or_default()
                .entry(parent)
                .or_default();
            if by_name.contains_key(&spec.name) {
                return Err(format!("subcommand `{}` is already registered", spec.name));
            }
            by_name.insert(spec.name.clone(), spec);
            return Ok(());
        }
        // Cheap duplicate check, then release the lock before the host call:
        // `register_command` reaches the core registry, and holding `roots`
        // across a host call would risk a lock-order inversion for no reason.
        if self.get(caller, &spec.name).is_some() {
            return Err(format!("command `{}` is already registered", spec.name));
        }
        // Register with the core registry first, so a core-name clash or a
        // duplicate is refused before a spec is stored. No lock is held here.
        let node = command_node(caller, &spec);
        if let Some(backend) = self.backend.get() {
            backend.register_command(caller, &spec.name, &node, spec.op)?;
        }
        let mut roots = self.roots.lock().unwrap();
        let by_name = roots.entry(caller.to_string()).or_default();
        // Re-check under the lock: a concurrent register that slipped past the
        // fast check above must not be silently overwritten.
        if by_name.contains_key(&spec.name) {
            return Err(format!("command `{}` is already registered", spec.name));
        }
        by_name.insert(spec.name.clone(), spec);
        Ok(())
    }

    fn get(&self, caller: &str, name: &str) -> Option<CommandSpec> {
        // Borrowed lookups take `&str` keys and allocate nothing.
        self.roots
            .lock()
            .unwrap()
            .get(caller)
            .and_then(|by_name| by_name.get(name))
            .cloned()
    }

    fn usage(&self, caller: &str, name: &str) -> String {
        self.get(caller, name).map(|s| s.usage).unwrap_or_default()
    }

    /// Resolve `name`/`args` to the effective spec. When `args[0]` names a
    /// subcommand of `name`, that subcommand and the remaining arguments are
    /// used; otherwise the root spec and all arguments are.
    fn resolve(&self, caller: &str, name: &str, args: &[String]) -> Resolution {
        let Some(root) = self.get(caller, name) else {
            return Resolution {
                subcommand: String::new(),
                rest: args.to_vec(),
                args: Vec::new(),
                usage: String::new(),
            };
        };
        let sub = args.first().and_then(|first| {
            self.subs
                .lock()
                .unwrap()
                .get(caller)
                .and_then(|by_parent| by_parent.get(name))
                .and_then(|by_name| by_name.get(first))
                .cloned()
        });
        let (subcommand, rest, spec): (String, Vec<String>, CommandSpec) = match sub {
            Some(sub) => (sub.name.clone(), args[1..].to_vec(), sub),
            None => (String::new(), args.to_vec(), root),
        };
        Resolution {
            subcommand,
            args: parse_against(&spec, &rest),
            rest,
            usage: spec.usage,
        }
    }

    fn parse(&self, caller: &str, name: &str, args: &[String]) -> Vec<(String, String)> {
        self.resolve(caller, name, args).args
    }
}

/// The permission node a spec registers under: its own, or `plugin.<id>.<name>`
/// when it names none — the same default the core registry's manifest path uses.
fn command_node(caller: &str, spec: &CommandSpec) -> String {
    match &spec.permission {
        Some(node) if !node.is_empty() => node.clone(),
        _ => format!("plugin.{caller}.{}", spec.name),
    }
}

/// Pair each declared argument with the supplied value, validating the scalar
/// kinds; extra values and values of the wrong kind are dropped.
fn parse_against(spec: &CommandSpec, args: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (declared, value) in spec.args.iter().zip(args.iter()) {
        if !value_is_valid(declared.kind, value) {
            continue;
        }
        out.push((declared.name.clone(), value.clone()));
    }
    out
}

impl CommandsService for Commands {
    fn register(&self, caller: &str, spec: CommandSpec) -> Result<(), String> {
        Commands::register(self, caller, spec)
    }
    fn usage(&self, caller: &str, name: &str) -> String {
        Commands::usage(self, caller, name)
    }
    fn parse(&self, caller: &str, name: &str, args: &[String]) -> Vec<(String, String)> {
        Commands::parse(self, caller, name, args)
    }
    fn resolve(&self, caller: &str, name: &str, args: &[String]) -> Resolution {
        Commands::resolve(self, caller, name, args)
    }
}

/// Whether `value` is plausible for an argument of `kind`; only the scalar
/// kinds are checked, `word`/`text` accept anything non-empty.
fn value_is_valid(kind: ArgKind, value: &str) -> bool {
    match kind {
        ArgKind::Integer => value.parse::<i64>().is_ok(),
        ArgKind::Word | ArgKind::Player | ArgKind::Item | ArgKind::Text => !value.is_empty(),
    }
}

// --- permissions -------------------------------------------------------

/// Permission checks, fronting the core resolver.
///
/// Read-only checks go through the host backend to the core permission store
/// (CMD-01): `check`/`require`/`level`/`groups` resolve against the player's
/// real groups and node defaults, and a host without a core fails closed (the
/// [`HostBackend`] defaults deny). The per-player **write** model (explicit
/// grants/denies, per-world context, meta) is CMD-02, so `grant`/`revoke`
/// refuse with a clear message rather than pretend the write happened — but
/// only after the `permissions.admin` gate, applied here by caller id, so a
/// native dependent is checked too.
struct Permissions {
    backend: Arc<dyn HostBackend>,
}

impl Permissions {
    fn new(backend: Arc<dyn HostBackend>) -> Permissions {
        Permissions { backend }
    }

    fn may(&self, caller: &str, capability: &str) -> bool {
        self.backend
            .capabilities(caller)
            .iter()
            .any(|c| c == capability)
    }
}

impl PermissionsService for Permissions {
    fn check(&self, _caller: &str, p: &Player, node: &str) -> bool {
        // No command op level here: the node's own default decides. A plugin
        // command's op level was already enforced by the core before
        // `on-command` is entered.
        self.backend.permission_check("sdk", p.id, node, 0)
    }
    fn require(&self, caller: &str, p: &Player, node: &str) -> bool {
        self.check(caller, p, node)
    }
    fn level(&self, _caller: &str, p: &Player) -> u8 {
        self.backend.permission_level("sdk", p.id)
    }
    fn groups(&self, _caller: &str, p: &Player) -> Vec<String> {
        self.backend.permission_groups("sdk", p.id)
    }
    fn meta(&self, _caller: &str, p: &Player, key: &str) -> Option<String> {
        self.backend.permission_meta("sdk", p.id, key)
    }
    fn grant(&self, caller: &str, subject: &str, node: &str, value: bool) -> Result<(), String> {
        if !self.may(caller, CAP_PERMISSIONS_ADMIN) {
            return Err(format!(
                "caller `{caller}` lacks the `{CAP_PERMISSIONS_ADMIN}` capability"
            ));
        }
        self.backend.permission_grant(caller, subject, node, value)
    }
    fn revoke(&self, caller: &str, subject: &str, node: &str) -> Result<(), String> {
        if !self.may(caller, CAP_PERMISSIONS_ADMIN) {
            return Err(format!(
                "caller `{caller}` lacks the `{CAP_PERMISSIONS_ADMIN}` capability"
            ));
        }
        self.backend.permission_revoke(caller, subject, node)
    }
}

// --- history -----------------------------------------------------------

/// Read-only history queries, gated on the caller's `history.read`
/// capability. The journal lives in the host backend; this service only
/// fronts it, and applies the capability gate in one place so a native
/// dependent and a WASM one are checked alike.
struct History {
    backend: Arc<dyn HostBackend>,
}

impl History {
    fn new(backend: Arc<dyn HostBackend>) -> History {
        History { backend }
    }

    /// Whether `caller` was granted `history.read`.
    fn may(&self, caller: &str) -> bool {
        self.backend
            .capabilities(caller)
            .iter()
            .any(|c| c == CAP_HISTORY_READ)
    }

    /// Refuse a query from an unprivileged caller, logging it once.
    fn deny(&self, caller: &str, what: &str) -> bool {
        if self.may(caller) {
            return false;
        }
        self.backend.log(
            caller,
            LogLevel::Warn,
            &format!("lacks `{CAP_HISTORY_READ}`; refused history {what}"),
        );
        true
    }
}

impl HistoryService for History {
    fn block_history(
        &self,
        caller: &str,
        x: i32,
        y: i32,
        z: i32,
        before: Option<u64>,
        limit: u32,
    ) -> HistoryPage {
        if self.deny(caller, "block-history") {
            return sdk::empty_history_page();
        }
        self.backend.history_block(caller, x, y, z, before, limit)
    }

    fn worldgen_original(&self, caller: &str, x: i32, y: i32, z: i32) -> bool {
        if self.deny(caller, "worldgen-original") {
            return false;
        }
        self.backend.history_worldgen_original(caller, x, y, z)
    }

    fn events(
        &self,
        caller: &str,
        window: HistoryWindow,
        before: Option<u64>,
        limit: u32,
    ) -> HistoryPage {
        if self.deny(caller, "events") {
            return sdk::empty_history_page();
        }
        self.backend.history_events(caller, window, before, limit)
    }

    fn item_chain(
        &self,
        caller: &str,
        uid: u64,
        depth: u32,
        before: Option<u64>,
        limit: u32,
    ) -> HistoryPage {
        if self.deny(caller, "item-chain") {
            return sdk::empty_history_page();
        }
        self.backend
            .history_item_chain(caller, uid, depth, before, limit)
    }

    fn container_log(
        &self,
        caller: &str,
        x: i32,
        y: i32,
        z: i32,
        before: Option<u64>,
        limit: u32,
    ) -> HistoryPage {
        if self.deny(caller, "container-log") {
            return sdk::empty_history_page();
        }
        self.backend
            .history_container_log(caller, x, y, z, before, limit)
    }

    fn player_events(
        &self,
        caller: &str,
        actor_low: u64,
        actor_high: u64,
        before: Option<u64>,
        limit: u32,
    ) -> HistoryPage {
        if self.deny(caller, "player-events") {
            return sdk::empty_history_page();
        }
        self.backend
            .history_player_events(caller, actor_low, actor_high, before, limit)
    }
}

// --- messages ----------------------------------------------------------

/// Message formatting and delivery. Colour translation and the (small,
/// built-in) translation table live here; the actual send goes through the
/// host backend, so a host without player access simply drops it.
struct Messages {
    backend: Arc<OnceLock<Arc<dyn HostBackend>>>,
    strings: HashMap<String, String>,
}

impl Messages {
    fn new(backend: Arc<OnceLock<Arc<dyn HostBackend>>>) -> Messages {
        let mut strings = HashMap::new();
        strings.insert(
            "sdk.no-permission".to_string(),
            "&cYou do not have permission for that.".to_string(),
        );
        strings.insert(
            "sdk.player-offline".to_string(),
            "&c{name} is not online.".to_string(),
        );
        Messages { backend, strings }
    }

    fn set_backend(&self, backend: Arc<dyn HostBackend>) {
        let _ = self.backend.set(backend);
    }

    /// Whether `caller` was granted `capability`, read from the host's record
    /// of effective grants. The one place the message gate is applied.
    fn may(&self, caller: &str, capability: &str) -> bool {
        self.backend
            .get()
            .map(|b| b.capabilities(caller).iter().any(|c| c == capability))
            .unwrap_or(false)
    }

    /// Refuse a send from a caller without `capability`, logging it once.
    fn deny(&self, caller: &str, capability: &str) -> bool {
        if self.may(caller, capability) {
            return false;
        }
        if let Some(backend) = self.backend.get() {
            backend.log(
                caller,
                LogLevel::Warn,
                &format!("lacks `{capability}`; message refused"),
            );
        }
        true
    }

    /// Look up a template and substitute `{name}` placeholders.
    fn translate(&self, key: &str, args: &[(String, String)]) -> String {
        let template = self
            .strings
            .get(key)
            .map(String::as_str)
            .unwrap_or(key)
            .to_string();
        let mut out = template;
        for (name, value) in args {
            out = out.replace(&format!("{{{name}}}"), value);
        }
        out
    }

    fn deliver(&self, p: &Player, text: &str, caller: &str) {
        let rendered = colorize(text);
        if let Some(backend) = self.backend.get() {
            backend.send_message(caller, p.id, &rendered);
        }
    }
}

impl MessagesService for Messages {
    fn send(&self, caller: &str, p: &Player, text: &str) {
        if self.deny(caller, CAP_MESSAGES_SEND) {
            return;
        }
        self.deliver(p, text, caller);
    }

    fn send_key(&self, caller: &str, p: &Player, key: &str, args: &[(String, String)]) {
        if self.deny(caller, CAP_MESSAGES_SEND) {
            return;
        }
        let rendered = self.translate(key, args);
        self.deliver(p, &rendered, caller);
    }

    fn actionbar(&self, caller: &str, _p: &Player, _text: &str) {
        // Action bars need the player/UI host API (CMD-32), absent here; the
        // capability is still checked so the gate is uniform.
        let _ = self.deny(caller, CAP_MESSAGES_SEND);
    }

    fn title(
        &self,
        caller: &str,
        _p: &Player,
        _title: &str,
        _subtitle: Option<&str>,
        _times: TitleTimes,
    ) {
        // Titles need the player/UI host API (CMD-32), absent here.
        let _ = self.deny(caller, CAP_MESSAGES_SEND);
    }

    fn broadcast(&self, caller: &str, text: &str) {
        if self.deny(caller, CAP_MESSAGES_BROADCAST) {
            return;
        }
        let rendered = colorize(text);
        if let Some(backend) = self.backend.get() {
            backend.broadcast(caller, &rendered);
        }
    }

    fn prefix(&self, _caller: &str, p: &Player) -> String {
        // The permission meta prefix (CMD-02), when the host provides one.
        self.backend
            .get()
            .and_then(|b| b.permission_meta("sdk", p.id, "prefix"))
            .unwrap_or_default()
    }
}

/// Translate `&`-style colour codes to the section-sign form the codec uses.
///
/// Pure and allocation-once; unknown `&x` sequences are left alone.
fn colorize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if let Some(&next) = chars.peek() {
                if is_color_code(next) {
                    out.push('\u{00a7}');
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

/// The vanilla colour and formatting codes after `&`/`§`.
fn is_color_code(c: char) -> bool {
    matches!(
        c,
        '0'..='9' | 'a'..='f' | 'k'..='o' | 'r' | 'A'..='F' | 'K'..='O' | 'R'
    )
}

// --- players -----------------------------------------------------------

/// Player lookup, fronting the host's player registry. Teleport and give need
/// core APIs (CMD-31/32) not on this branch, so they answer neutrally.
struct Players {
    backend: Arc<OnceLock<Arc<dyn HostBackend>>>,
}

impl Players {
    fn new(backend: Arc<OnceLock<Arc<dyn HostBackend>>>) -> Players {
        Players { backend }
    }

    fn set_backend(&self, backend: Arc<dyn HostBackend>) {
        let _ = self.backend.set(backend);
    }

    fn online(&self, caller: &str) -> Vec<Player> {
        self.backend
            .get()
            .map(|b| b.online_players(caller))
            .unwrap_or_default()
    }
}

impl PlayersService for Players {
    fn online(&self, caller: &str) -> Vec<Player> {
        self.online(caller)
    }

    fn by_name(&self, caller: &str, name: &str) -> Option<Player> {
        self.online(caller)
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    fn by_id(&self, caller: &str, id: u64) -> Option<Player> {
        self.online(caller).into_iter().find(|p| p.id == id)
    }

    fn offline_name(&self, _caller: &str, _low: u64, _high: u64) -> Option<String> {
        // The offline player store is not exposed yet.
        None
    }

    fn teleport(
        &self,
        caller: &str,
        p: &Player,
        to: Pos,
        cause: TeleportCause,
    ) -> Result<(), String> {
        match self.backend.get() {
            Some(b) => b.teleport_player(caller, p.id, to.x, to.y, to.z, cause),
            None => Err("player service unavailable".to_string()),
        }
    }

    fn give(&self, _caller: &str, _p: &Player, _item: &str, count: u32) -> u64 {
        // Nothing could be given, so all of it is "left over".
        count as u64
    }
}

// --- placeholders ------------------------------------------------------

/// The shared placeholder registry (`sdk.placeholders`): named providers
/// (native closures and WASM-pushed per-player values), the built-ins, and a
/// short-lived cache so a provider is not called for every consumer on every
/// redraw. The sidebar, menus and chat all resolve through this one instance.
struct Placeholders {
    backend: Arc<OnceLock<Arc<dyn HostBackend>>>,
    state: RwLock<PlaceholderState>,
    /// Per-player query, reused within the tick it was built. Cleared each
    /// tick so per-tick facts (position, ping) refresh.
    queries: Mutex<HashMap<u64, (u64, Arc<PlaceholderQuery>)>>,
    /// The current world tick, advanced by the SDK's own `on_tick`.
    tick: AtomicU64,
    /// How long a resolved token is reused, in ticks. `0` disables the cache.
    ttl: AtomicU64,
}

#[derive(Default)]
struct PlaceholderState {
    /// Native closures, by token name.
    providers: HashMap<String, PlaceholderFn>,
    /// Names declared by a WASM caller (before any value arrives).
    declared: HashSet<String>,
    /// Pushed values: token → player id → last value.
    values: HashMap<String, HashMap<u64, String>>,
    /// Which caller owns each custom token, so one plugin cannot overwrite
    /// another's `%name%`. A built-in is never shown here (it is reserved).
    owners: HashMap<String, String>,
    /// Resolved-token cache: player id → token → (value, tick).
    cache: HashMap<u64, HashMap<String, (String, u64)>>,
}

impl Placeholders {
    fn new(backend: Arc<OnceLock<Arc<dyn HostBackend>>>) -> Placeholders {
        Placeholders {
            backend,
            state: RwLock::new(PlaceholderState::default()),
            queries: Mutex::new(HashMap::new()),
            tick: AtomicU64::new(0),
            // A second's worth, matching the sidebar's 4 Hz redraw.
            ttl: AtomicU64::new(20),
        }
    }

    fn set_backend(&self, backend: Arc<dyn HostBackend>) {
        let _ = self.backend.set(backend);
    }

    fn set_ttl(&self, ticks: u64) {
        self.ttl.store(ticks, Ordering::Relaxed);
    }

    /// Advance the clock and drop the per-tick query memo.
    fn advance(&self, n: u64) {
        self.tick.store(n, Ordering::Relaxed);
        self.queries.lock().unwrap().clear();
    }

    fn now(&self) -> u64 {
        self.tick.load(Ordering::Relaxed)
    }

    /// The query for `player`, built once per tick and reused across tokens.
    fn query_for(&self, caller: &str, p: &Player) -> Arc<PlaceholderQuery> {
        let tick = self.now();
        {
            let map = self.queries.lock().unwrap();
            if let Some((t, q)) = map.get(&p.id) {
                if *t == tick {
                    return Arc::clone(q);
                }
            }
        }
        let q = Arc::new(self.build_query(caller, p));
        self.queries
            .lock()
            .unwrap()
            .insert(p.id, (tick, Arc::clone(&q)));
        q
    }

    fn build_query(&self, caller: &str, p: &Player) -> PlaceholderQuery {
        let b = self.backend.get();
        PlaceholderQuery {
            player: p.id,
            name: p.name.clone(),
            extra: b.and_then(|b| b.player_extra(caller, p.id)),
            online: b.map(|b| b.online_players(caller).len()).unwrap_or(0),
            max_players: b.map(|b| b.max_players(caller)).unwrap_or(20),
            tps: b.map(|b| b.server_tps(caller)).unwrap_or(20.0),
            time: b.map(|b| b.world_time(caller)).unwrap_or(0),
            unix_time: unix_now(),
        }
    }

    /// Resolve `text` for `player`: one pass, a token at a time, each resolved
    /// token cached. An unmatched `%` and an unknown token are left literal.
    fn substitute(&self, caller: &str, p: &Player, text: &str) -> String {
        if !text.contains('%') {
            return text.to_string();
        }
        let q = self.query_for(caller, p);
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find('%') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            match after.find('%') {
                Some(end) => {
                    let token = &after[..end];
                    match self.resolve_token(p.id, token, &q) {
                        Some(v) => out.push_str(&v),
                        None => {
                            // Keep the literal, `%` included, so a typo shows.
                            out.push('%');
                            out.push_str(token);
                            out.push('%');
                        }
                    }
                    rest = &after[end + 1..];
                }
                None => {
                    out.push_str(&rest[start..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// One token's value, honouring the cache.
    fn resolve_token(&self, player: u64, token: &str, q: &PlaceholderQuery) -> Option<String> {
        let tick = self.now();
        let ttl = self.ttl.load(Ordering::Relaxed);
        if ttl > 0 {
            let st = self.state.read().unwrap();
            if let Some((v, at)) = st.cache.get(&player).and_then(|m| m.get(token)) {
                if tick.wrapping_sub(*at) < ttl {
                    return Some(v.clone());
                }
            }
        }
        let v = self.compute(player, token, q);
        if ttl > 0 {
            if let Some(v) = &v {
                self.state
                    .write()
                    .unwrap()
                    .cache
                    .entry(player)
                    .or_default()
                    .insert(token.to_string(), (v.clone(), tick));
            }
        }
        v
    }

    /// The uncached value: a native provider, then a pushed value, then a
    /// built-in. The provider is cloned out before it is called, so a closure
    /// that re-enters the registry cannot deadlock the lock.
    fn compute(&self, player: u64, token: &str, q: &PlaceholderQuery) -> Option<String> {
        let provider = self.state.read().unwrap().providers.get(token).cloned();
        if let Some(provider) = provider {
            if let Some(v) = provider(q) {
                return Some(v);
            }
        }
        let pushed = self
            .state
            .read()
            .unwrap()
            .values
            .get(token)
            .and_then(|m| m.get(&player))
            .cloned();
        if pushed.is_some() {
            return pushed;
        }
        builtin(token, q)
    }

    /// Log a refused registration/push, so a spoofing attempt is visible in
    /// the server log rather than silently ignored.
    fn refuse(&self, caller: &str, name: &str, why: &str) {
        if let Some(b) = self.backend.get() {
            b.log(
                caller,
                LogLevel::Warn,
                &format!("placeholder `%{name}%` refused: {why}"),
            );
        }
    }
}

/// The built-in token names. A plugin may not push a value under one of these
/// (a native closure still may, being trusted-by-install).
fn is_builtin(token: &str) -> bool {
    matches!(
        token,
        "player"
            | "name"
            | "displayname"
            | "display_name"
            | "online"
            | "max"
            | "max_players"
            | "tps"
            | "ping"
            | "world"
            | "x"
            | "y"
            | "z"
            | "health"
            | "level"
            | "time"
            | "world_time"
            | "server_time"
    )
}

impl PlaceholdersService for Placeholders {
    fn register(&self, caller: &str, name: String, provider: PlaceholderFn) -> bool {
        let mut st = self.state.write().unwrap();
        if let Some(owner) = st.owners.get(&name).cloned() {
            if owner != caller {
                drop(st);
                self.refuse(caller, &name, &format!("owned by `{owner}`"));
                return false;
            }
        }
        st.providers.insert(name.clone(), provider);
        st.owners.insert(name, caller.to_string());
        // A new provider must be visible immediately, not after the cache
        // expires.
        st.cache.clear();
        true
    }

    fn register_name(&self, caller: &str, name: String) -> bool {
        if is_builtin(&name) {
            self.refuse(caller, &name, "reserved built-in");
            return false;
        }
        let mut st = self.state.write().unwrap();
        if let Some(owner) = st.owners.get(&name).cloned() {
            if owner != caller {
                drop(st);
                self.refuse(caller, &name, &format!("owned by `{owner}`"));
                return false;
            }
        }
        st.declared.insert(name.clone());
        st.owners.insert(name, caller.to_string());
        true
    }

    fn set_value(&self, caller: &str, name: String, player: u64, value: String) {
        if is_builtin(&name) {
            self.refuse(caller, &name, "reserved built-in");
            return;
        }
        let tick = self.now();
        let mut st = self.state.write().unwrap();
        if let Some(owner) = st.owners.get(&name).cloned() {
            if owner != caller {
                drop(st);
                self.refuse(caller, &name, &format!("owned by `{owner}`"));
                return;
            }
        }
        st.declared.insert(name.clone());
        st.owners
            .entry(name.clone())
            .or_insert_with(|| caller.to_string());
        st.values
            .entry(name.clone())
            .or_default()
            .insert(player, value.clone());
        st.cache
            .entry(player)
            .or_default()
            .insert(name, (value, tick));
    }

    fn resolve(&self, caller: &str, player: &Player, text: &str) -> String {
        self.substitute(caller, player, text)
    }
}

/// The built-in placeholders, answered from the host's per-player view.
fn builtin(token: &str, q: &PlaceholderQuery) -> Option<String> {
    let extra = q.extra.as_ref();
    Some(match token {
        "player" | "name" => q.name.clone(),
        "displayname" | "display_name" => extra
            .map(|e| e.display_name.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| q.name.clone()),
        "online" => q.online.to_string(),
        "max" | "max_players" => q.max_players.to_string(),
        "tps" => format!("{:.1}", q.tps),
        "ping" => extra.map(|e| e.ping.to_string())?,
        "world" => extra.map(|e| e.world.clone())?,
        "x" => extra.map(|e| e.x.floor() as i64)?.to_string(),
        "y" => extra.map(|e| e.y.floor() as i64)?.to_string(),
        "z" => extra.map(|e| e.z.floor() as i64)?.to_string(),
        "health" => extra.map(|e| format!("{:.1}", e.health))?,
        "level" => extra.map(|e| e.level.to_string())?,
        "time" | "world_time" => world_clock(q.time),
        "server_time" => wall_clock(q.unix_time),
        _ => return None,
    })
}

/// Format a world time in ticks as `HH:MM` (Minecraft's day starts at 06:00).
fn world_clock(time: i64) -> String {
    let time = time.rem_euclid(24_000);
    let hours = (time / 1000 + 6) % 24;
    let minutes = (time % 1000) * 60 / 1000;
    format!("{hours:02}:{minutes:02}")
}

/// Format a Unix timestamp as `HH:MM` in UTC.
fn wall_clock(unix: i64) -> String {
    let secs = unix.rem_euclid(86_400);
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}

/// The current Unix time in seconds.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// --- scheduler / cooldowns / confirm -----------------------------------

/// The current world tick, shared by the timer-driven services. Advanced once
/// per tick by [`SdkPlugin::on_tick`].
#[derive(Default)]
struct Clock {
    tick: AtomicU64,
}

impl Clock {
    fn now(&self) -> u64 {
        self.tick.load(Ordering::Relaxed)
    }

    fn set(&self, n: u64) {
        self.tick.store(n, Ordering::Relaxed);
    }
}

struct Timer {
    caller: Arc<str>,
    due: u64,
    period: Option<u64>,
}

/// The timer state, kept behind one lock so the heap, the timers and the due
/// queue never disagree.
#[derive(Default)]
struct SchedulerState {
    timers: HashMap<u64, Timer>,
    /// Min-heap of `(due, id)`. An idle tick is a single `peek`, so advancing
    /// costs O(1) when nothing is due and O(due · log n) otherwise — never a
    /// scan of every live timer.
    heap: BinaryHeap<Reverse<(u64, u64)>>,
    /// `(id, caller)` pairs that came due since the last `poll`, retaining the
    /// caller so a poll by one dependent leaves another's due timers for them.
    due: Vec<(u64, Arc<str>)>,
}

/// The timer wheel. Ids are global and monotonically increasing; a timer is
/// owned by the caller that created it, so one dependent can neither see nor
/// cancel another's.
struct Scheduler {
    clock: Arc<Clock>,
    next: AtomicU64,
    state: Mutex<SchedulerState>,
}

impl Scheduler {
    fn new(clock: Arc<Clock>) -> Scheduler {
        Scheduler {
            clock,
            next: AtomicU64::new(0),
            state: Mutex::new(SchedulerState::default()),
        }
    }

    fn schedule(&self, caller: &str, due: u64, period: Option<u64>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let mut st = self.state.lock().unwrap();
        st.timers.insert(
            id,
            Timer {
                caller: Arc::from(caller),
                due,
                period,
            },
        );
        st.heap.push(Reverse((due, id)));
        id
    }

    fn advance(&self, n: u64) {
        self.clock.set(n);
        let mut st = self.state.lock().unwrap();
        while let Some(Reverse((due, id))) = st.heap.peek().copied() {
            if due > n {
                break;
            }
            st.heap.pop();
            // Read the timer and its reschedule decision, then drop the borrow
            // before touching the other fields of `st`.
            let (caller, next) = {
                let Some(timer) = st.timers.get_mut(&id) else {
                    // Cancelled (or already spent); drop the stale entry.
                    continue;
                };
                // A repeat that rescheduled pushed a fresh entry; an older one
                // that is still in the heap must not fire twice.
                if timer.due != due {
                    continue;
                }
                let caller = Arc::clone(&timer.caller);
                let next = match timer.period {
                    Some(period) if period > 0 => {
                        // Catch up without spinning if several periods passed.
                        let behind = n - timer.due;
                        timer.due += (behind / period + 1) * period;
                        Some(timer.due)
                    }
                    // A one-shot is spent; drop it so it cannot fire again.
                    _ => None,
                };
                (caller, next)
            };
            match next {
                Some(next) => st.heap.push(Reverse((next, id))),
                None => {
                    st.timers.remove(&id);
                }
            }
            st.due.push((id, caller));
        }
    }

    fn cancel(&self, caller: &str, id: u64) -> bool {
        let mut st = self.state.lock().unwrap();
        match st.timers.get(&id) {
            Some(t) if t.caller.as_ref() == caller => {
                st.timers.remove(&id);
                // The heap entry is left and skipped on pop; a due entry not
                // yet polled is dropped here.
                st.due.retain(|(d, _)| *d != id);
                true
            }
            _ => false,
        }
    }

    fn poll(&self, caller: &str) -> Vec<u64> {
        let mut st = self.state.lock().unwrap();
        let mut mine = Vec::new();
        let mut rest = Vec::new();
        for (id, owner) in st.due.drain(..) {
            if owner.as_ref() == caller {
                mine.push(id);
            } else {
                rest.push((id, owner));
            }
        }
        st.due = rest;
        mine
    }
}

impl Scheduler {
    fn after(&self, caller: &str, ticks: u64) -> u64 {
        self.schedule(caller, self.clock.now() + ticks.max(1), None)
    }

    fn every(&self, caller: &str, period: u64) -> u64 {
        let period = period.max(1);
        self.schedule(caller, self.clock.now() + period, Some(period))
    }
}

impl SchedulerService for Scheduler {
    fn after(&self, caller: &str, ticks: u64) -> u64 {
        Scheduler::after(self, caller, ticks)
    }
    fn every(&self, caller: &str, period: u64) -> u64 {
        Scheduler::every(self, caller, period)
    }
    fn cancel(&self, caller: &str, id: u64) -> bool {
        Scheduler::cancel(self, caller, id)
    }
    fn poll(&self, caller: &str) -> Vec<u64> {
        Scheduler::poll(self, caller)
    }
}

/// Per-caller, per-key cooldowns: `(armed_tick, duration_ticks)`.
///
/// Nested so a lookup takes `&str` keys and allocates nothing on the query
/// path; only arming a cooldown allocates.
struct Cooldowns {
    clock: Arc<Clock>,
    map: Mutex<HashMap<String, HashMap<String, (u64, u64)>>>,
}

impl Cooldowns {
    fn new(clock: Arc<Clock>) -> Cooldowns {
        Cooldowns {
            clock,
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl CooldownsService for Cooldowns {
    fn ready(&self, caller: &str, key: &str, ticks: u64) -> bool {
        let now = self.clock.now();
        let mut map = self.map.lock().unwrap();
        // Look up with borrowed keys; only reach for the entry API on a miss.
        if let Some((armed, duration)) = map.get(caller).and_then(|m| m.get(key)) {
            if now < *armed + *duration {
                return false;
            }
        }
        map.entry(caller.to_string())
            .or_default()
            .insert(key.to_string(), (now, ticks.max(1)));
        true
    }

    fn remaining(&self, caller: &str, key: &str) -> u64 {
        let now = self.clock.now();
        let map = self.map.lock().unwrap();
        match map.get(caller).and_then(|m| m.get(key)) {
            Some((armed, duration)) => (*armed + *duration).saturating_sub(now),
            None => 0,
        }
    }
}

/// Pending "type again to confirm" prompts, expiring after 30 seconds.
struct Confirm {
    clock: Arc<Clock>,
    messages: Arc<Messages>,
    pending: Mutex<HashMap<(String, u64, String), u64>>,
}

/// How long a confirmation stays valid, in ticks (30s at 20 tps).
const CONFIRM_TICKS: u64 = 600;

impl Confirm {
    fn new(clock: Arc<Clock>, messages: Arc<Messages>) -> Confirm {
        Confirm {
            clock,
            messages,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Drop expired entries; returns the caller's live entries count for
    /// `player`.
    fn live_for(&self, caller: &str, player: u64) -> usize {
        let now = self.clock.now();
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, expires| *expires > now);
        pending
            .keys()
            .filter(|(c, p, _)| c == caller && *p == player)
            .count()
    }
}

impl ConfirmService for Confirm {
    fn request(&self, caller: &str, p: &Player, prompt: &str, token: &str) -> bool {
        if self.live_for(caller, p.id) > 0 {
            return false;
        }
        let expires = self.clock.now() + CONFIRM_TICKS;
        self.pending
            .lock()
            .unwrap()
            .insert((caller.to_string(), p.id, token.to_string()), expires);
        self.messages.send(caller, p, prompt);
        true
    }

    fn check(&self, caller: &str, p: &Player, token: &str) -> bool {
        let now = self.clock.now();
        let mut pending = self.pending.lock().unwrap();
        match pending.get(&(caller.to_string(), p.id, token.to_string())) {
            Some(expires) if *expires > now => {
                pending.remove(&(caller.to_string(), p.id, token.to_string()));
                true
            }
            Some(_) => {
                pending.remove(&(caller.to_string(), p.id, token.to_string()));
                false
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_plugin::ServiceRegistry;
    use std::sync::Mutex as StdMutex;

    /// A backend that grants one caller the given capabilities and nobody else,
    /// for exercising the service-layer capability gate.
    struct CapsBackend {
        caller: String,
        caps: Vec<String>,
    }

    impl HostBackend for CapsBackend {
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
        fn capabilities(&self, plugin: &str) -> Vec<String> {
            if plugin == self.caller {
                self.caps.clone()
            } else {
                Vec::new()
            }
        }
    }

    fn caps_backend(caller: &str, caps: &[&str]) -> Arc<dyn HostBackend> {
        Arc::new(CapsBackend {
            caller: caller.to_string(),
            caps: caps.iter().map(|c| (*c).to_string()).collect(),
        })
    }

    fn spec(name: &str, usage: &str, args: Vec<(&str, ArgKind)>) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            parent: None,
            permission: None,
            op: 0,
            usage: usage.to_string(),
            description: String::new(),
            args: args
                .into_iter()
                .map(|(n, kind)| sdk::Arg {
                    name: n.to_string(),
                    kind,
                    required: true,
                    choices: Vec::new(),
                    help: String::new(),
                })
                .collect(),
        }
    }

    fn sub(parent: &str, name: &str, usage: &str, args: Vec<(&str, ArgKind)>) -> CommandSpec {
        let mut s = spec(name, usage, args);
        s.parent = Some(parent.to_string());
        s
    }

    #[test]
    fn a_subcommand_is_stored_under_its_parent_not_registered_top_level() {
        let commands = Commands::default();
        commands
            .register("p", spec("home", "/home", vec![]))
            .unwrap();
        commands
            .register(
                "p",
                sub(
                    "home",
                    "set",
                    "/home set <name>",
                    vec![("name", ArgKind::Word)],
                ),
            )
            .unwrap();
        // The root is a top-level command; the subcommand is not.
        assert!(commands.get("p", "home").is_some());
        assert!(commands.get("p", "set").is_none());
        // Resolving the root with the subcommand word selects the subcommand.
        let r = commands.resolve("p", "home", &["set".to_string(), "base".to_string()]);
        assert_eq!(r.subcommand, "set");
        assert_eq!(r.rest, vec!["base".to_string()]);
        assert_eq!(r.usage, "/home set <name>");
        assert_eq!(r.args, vec![("name".to_string(), "base".to_string())]);
    }

    #[test]
    fn resolve_falls_back_to_the_root_when_no_subcommand_matches() {
        let commands = Commands::default();
        commands
            .register(
                "p",
                spec("home", "/home [name]", vec![("name", ArgKind::Word)]),
            )
            .unwrap();
        commands
            .register(
                "p",
                sub(
                    "home",
                    "set",
                    "/home set <name>",
                    vec![("name", ArgKind::Word)],
                ),
            )
            .unwrap();
        // An argument that is not a subcommand is a root argument.
        let r = commands.resolve("p", "home", &["base".to_string()]);
        assert_eq!(r.subcommand, "");
        assert_eq!(r.rest, vec!["base".to_string()]);
        assert_eq!(r.usage, "/home [name]");
        assert_eq!(r.args, vec![("name".to_string(), "base".to_string())]);
        // No arguments resolves to the root spec.
        let r = commands.resolve("p", "home", &[]);
        assert_eq!(r.subcommand, "");
        assert!(r.args.is_empty());
    }

    #[test]
    fn a_subcommand_never_reaches_the_core_registry() {
        // A backend recording every core registration.
        #[derive(Default)]
        struct Core {
            registered: StdMutex<Vec<String>>,
        }
        impl HostBackend for Core {
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
            fn register_command(
                &self,
                _plugin: &str,
                name: &str,
                _node: &str,
                _op: u8,
            ) -> Result<(), String> {
                self.registered.lock().unwrap().push(name.to_string());
                Ok(())
            }
        }
        let core = Arc::new(Core::default());
        let slot = Arc::new(OnceLock::new());
        let commands = Commands::new(Arc::clone(&slot));
        let _ = slot.set(core.clone() as Arc<dyn HostBackend>);
        commands
            .register("p", spec("home", "/home", vec![]))
            .unwrap();
        commands
            .register("p", sub("home", "set", "/home set", vec![]))
            .unwrap();
        assert_eq!(core.registered.lock().unwrap().as_slice(), ["home"]);
    }

    #[test]
    fn register_does_not_hold_a_lock_across_the_host_call() {
        // A backend whose `register_command` re-enters `register` on the same
        // `Commands`: with the `specs` lock held across the host call this
        // would deadlock; with the fix it completes.
        struct Reentrant {
            commands: StdMutex<Option<Arc<Commands>>>,
        }
        impl HostBackend for Reentrant {
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
            fn register_command(
                &self,
                plugin: &str,
                name: &str,
                _node: &str,
                _op: u8,
            ) -> Result<(), String> {
                if name == "outer" {
                    // Re-enter while the host call is in flight.
                    let commands = self.commands.lock().unwrap().clone().unwrap();
                    commands
                        .register(plugin, spec("inner", "/inner", vec![]))
                        .unwrap();
                }
                Ok(())
            }
        }

        let slot = Arc::new(OnceLock::new());
        let commands = Arc::new(Commands::new(Arc::clone(&slot)));
        let backend = Arc::new(Reentrant {
            commands: StdMutex::new(Some(Arc::clone(&commands))),
        });
        let _ = slot.set(backend as Arc<dyn HostBackend>);

        commands
            .register("p", spec("outer", "/outer", vec![]))
            .unwrap();
        assert!(commands.get("p", "outer").is_some());
        assert!(commands.get("p", "inner").is_some());
    }

    #[test]
    fn a_registered_command_parses_its_arguments() {
        let commands = Commands::default();
        commands
            .register(
                "home-plugin",
                spec("home", "/home <name>", vec![("name", ArgKind::Word)]),
            )
            .unwrap();
        assert_eq!(commands.usage("home-plugin", "home"), "/home <name>");
        let parsed = commands.parse("home-plugin", "home", &["base".to_string()]);
        assert_eq!(parsed, vec![("name".to_string(), "base".to_string())]);
    }

    #[test]
    fn commands_are_scoped_to_their_caller() {
        let commands = Commands::default();
        commands
            .register("a", spec("home", "/home", vec![]))
            .unwrap();
        assert!(commands.get("b", "home").is_none());
        assert_eq!(commands.usage("b", "home"), "");
        assert!(commands.parse("b", "home", &[]).is_empty());
    }

    #[test]
    fn a_duplicate_command_is_refused() {
        let commands = Commands::default();
        commands
            .register("a", spec("home", "/home", vec![]))
            .unwrap();
        let err = commands
            .register("a", spec("home", "/home", vec![]))
            .unwrap_err();
        assert!(err.contains("already registered"), "{err}");
    }

    #[test]
    fn a_bad_integer_argument_is_dropped_not_misparsed() {
        let commands = Commands::default();
        commands
            .register(
                "a",
                spec("pay", "/pay <amount>", vec![("amount", ArgKind::Integer)]),
            )
            .unwrap();
        assert!(commands.parse("a", "pay", &["ten".to_string()]).is_empty());
        assert_eq!(
            commands.parse("a", "pay", &["10".to_string()]),
            vec![("amount".to_string(), "10".to_string())]
        );
    }

    #[test]
    fn the_default_node_is_derived_from_the_caller_and_name() {
        let spec = spec("home", "/home", vec![]);
        assert_eq!(command_node("essentials", &spec), "plugin.essentials.home");
        let mut named = spec;
        named.permission = Some("essentials.home.use".to_string());
        assert_eq!(command_node("essentials", &named), "essentials.home.use");
    }

    #[test]
    fn register_forwards_to_the_core_command_registry() {
        // A backend that records every registration and refuses a core name,
        // standing in for the server's command registry.
        #[derive(Default)]
        struct Core {
            registered: StdMutex<Vec<(String, String, String, u8)>>,
        }
        impl HostBackend for Core {
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
            fn register_command(
                &self,
                plugin: &str,
                name: &str,
                node: &str,
                op: u8,
            ) -> Result<(), String> {
                if name == "give" {
                    return Err("`/give` is a core command".to_string());
                }
                self.registered.lock().unwrap().push((
                    plugin.to_string(),
                    name.to_string(),
                    node.to_string(),
                    op,
                ));
                Ok(())
            }
        }

        let core = Arc::new(Core::default());
        let slot = Arc::new(OnceLock::new());
        let commands = Commands::new(Arc::clone(&slot));
        let _ = slot.set(core.clone() as Arc<dyn HostBackend>);

        let mut s = spec("home", "/home <name>", vec![("name", ArgKind::Word)]);
        s.permission = Some("essentials.home".to_string());
        s.op = 2;
        commands.register("essentials", s).unwrap();
        assert_eq!(
            core.registered.lock().unwrap().as_slice(),
            [(
                "essentials".to_string(),
                "home".to_string(),
                "essentials.home".to_string(),
                2
            )]
        );

        // A core name is refused by the registry and no spec is stored.
        let err = commands
            .register("essentials", spec("give", "/give", vec![]))
            .unwrap_err();
        assert!(err.contains("core"), "{err}");
        assert!(commands.get("essentials", "give").is_none());
    }

    #[test]
    fn the_permission_service_fails_closed_without_a_core() {
        // A backend with no permission core (the `HostBackend` defaults) must
        // answer deny/zero/empty, never guess, even for an operator player.
        let p = Permissions::new(caps_backend("greet", &[CAP_PERMISSIONS_ADMIN]));
        let player = Player {
            id: 1,
            name: "Steve".to_string(),
            operator: true,
        };
        assert!(!p.check("greet", &player, "essentials.home"));
        assert!(!p.require("greet", &player, "essentials.home"));
        assert_eq!(p.level("greet", &player), 0);
        assert!(!p.has_level("greet", &player, 2));
        assert!(p.groups("greet", &player).is_empty());
        assert!(p.meta("greet", &player, "prefix").is_none());
        // The write model is CMD-02, so a write refuses even when the
        // capability is granted.
        assert!(p.grant("greet", "Steve", "n", true).is_err());
        assert!(p.revoke("greet", "Steve", "n").is_err());
    }

    #[test]
    fn the_permission_admin_gate_is_applied_in_the_service_layer() {
        // A caller lacking `permissions.admin` is refused with a capability
        // reason, before the write-model reason.
        let p = Permissions::new(caps_backend("greet", &[]));
        let err = p.grant("greet", "Steve", "n", true).unwrap_err();
        assert!(err.contains("permissions.admin"), "{err}");
        // A caller that has it gets the missing-write-model reason instead.
        let p = Permissions::new(caps_backend("greet", &[CAP_PERMISSIONS_ADMIN]));
        let err = p.grant("greet", "Steve", "n", true).unwrap_err();
        assert!(err.contains("CMD-02"), "{err}");
    }

    #[test]
    fn colour_codes_translate_and_unknown_amps_survive() {
        assert_eq!(
            colorize("&aHello &lworld&r!"),
            "\u{00a7}aHello \u{00a7}lworld\u{00a7}r!"
        );
        // A bare `&`, an unknown code and a trailing `&` are untouched.
        assert_eq!(colorize("a & b"), "a & b");
        assert_eq!(colorize("&z"), "&z");
        assert_eq!(colorize("trailing &"), "trailing &");
    }

    #[test]
    fn a_translation_key_substitutes_its_arguments() {
        let backend = Arc::new(OnceLock::new());
        let messages = Messages::new(backend);
        assert_eq!(
            messages.translate(
                "sdk.player-offline",
                &[("name".to_string(), "Alex".to_string())]
            ),
            "&cAlex is not online."
        );
        // A missing key falls back to the key itself, so it is visible.
        assert_eq!(messages.translate("plugin.custom", &[]), "plugin.custom");
    }

    #[test]
    fn a_send_reaches_the_backend_with_colour_applied() {
        // A recording backend, to prove the message path uses the host.
        #[derive(Default)]
        struct Rec {
            sent: StdMutex<Vec<(u64, String)>>,
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
            fn send_message(&self, _plugin: &str, player: u64, text: &str) {
                self.sent.lock().unwrap().push((player, text.to_string()));
            }
            fn capabilities(&self, plugin: &str) -> Vec<String> {
                if plugin == "greet" {
                    vec![CAP_MESSAGES_SEND.to_string()]
                } else {
                    Vec::new()
                }
            }
        }
        let rec = Arc::new(Rec::default());
        let slot = Arc::new(OnceLock::new());
        let messages = Messages::new(Arc::clone(&slot));
        let _ = slot.set(rec.clone() as Arc<dyn HostBackend>);
        let player = Player {
            id: 7,
            name: "Steve".to_string(),
            operator: false,
        };
        messages.send("greet", &player, "&aHi");
        assert_eq!(
            rec.sent.lock().unwrap().as_slice(),
            [(7u64, "\u{00a7}aHi".to_string())]
        );
        // A caller without the capability sends nothing (the gate lives here).
        messages.send("nobody", &player, "&cblocked");
        assert_eq!(rec.sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn player_lookup_uses_the_backend() {
        #[derive(Default)]
        struct Rec;
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
            fn online_players(&self, _plugin: &str) -> Vec<Player> {
                vec![Player {
                    id: 7,
                    name: "Steve".to_string(),
                    operator: false,
                }]
            }
        }
        let slot = Arc::new(OnceLock::new());
        let players = Players::new(Arc::clone(&slot));
        let _ = slot.set(Arc::new(Rec) as Arc<dyn HostBackend>);
        assert_eq!(players.online("p").len(), 1);
        assert_eq!(players.by_name("p", "steve").unwrap().id, 7);
        assert!(players.by_name("p", "nobody").is_none());
        assert!(players
            .teleport(
                "p",
                &players.online("p")[0],
                Pos {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0
                },
                TeleportCause::Plugin
            )
            .is_err());
    }

    #[test]
    fn init_registers_every_service() {
        let registry = ServiceRegistry::new();
        let backend: Arc<OnceLock<Arc<dyn HostBackend>>> = Arc::new(OnceLock::new());
        let clock = Arc::new(Clock::default());
        let messages = Arc::new(Messages::new(Arc::clone(&backend)));
        let messages_svc: Arc<dyn MessagesService> = messages.clone();
        sdk::register_commands(&registry, Arc::new(Commands::default())).unwrap();
        sdk::register_permissions(
            &registry,
            Arc::new(Permissions::new(caps_backend("x", &[]))),
        )
        .unwrap();
        sdk::register_messages(&registry, messages_svc).unwrap();
        sdk::register_players(&registry, Arc::new(Players::new(backend))).unwrap();
        sdk::register_scheduler(&registry, Arc::new(Scheduler::new(Arc::clone(&clock)))).unwrap();
        sdk::register_cooldowns(&registry, Arc::new(Cooldowns::new(Arc::clone(&clock)))).unwrap();
        sdk::register_confirm(&registry, Arc::new(Confirm::new(clock, messages))).unwrap();
        sdk::register_history(&registry, Arc::new(History::new(caps_backend("x", &[])))).unwrap();
        assert!(sdk::commands(&registry).is_some());
        assert!(sdk::permissions(&registry).is_some());
        assert!(sdk::messages(&registry).is_some());
        assert!(sdk::players(&registry).is_some());
        assert!(sdk::scheduler(&registry).is_some());
        assert!(sdk::cooldowns(&registry).is_some());
        assert!(sdk::confirm(&registry).is_some());
        assert!(sdk::history(&registry).is_some());
    }

    #[test]
    fn history_reads_are_gated_on_the_capability() {
        // A caller without `history.read` gets the neutral answer, never a
        // backend call: the gate is checked before forwarding.
        let denied = History::new(caps_backend("outsider", &[]));
        assert!(!denied.may("outsider"));
        assert!(denied
            .block_history("outsider", 1, 2, 3, None, 10)
            .events
            .is_empty());
        assert!(!denied.worldgen_original("outsider", 1, 2, 3));
        assert!(denied
            .item_chain("outsider", 1, 1, None, 10)
            .events
            .is_empty());
        assert!(denied
            .container_log("outsider", 1, 2, 3, None, 10)
            .events
            .is_empty());
        assert!(denied
            .player_events("outsider", 0, 0, None, 10)
            .events
            .is_empty());

        // A granted caller is forwarded; the test backend has no journal, so
        // the neutral answer comes from the backend, not from the gate.
        let allowed = History::new(caps_backend("reader", &[CAP_HISTORY_READ]));
        assert!(allowed.may("reader"));
        assert!(allowed
            .block_history("reader", 1, 2, 3, None, 10)
            .events
            .is_empty());
    }

    #[test]
    fn a_one_shot_timer_fires_once_at_its_tick() {
        let clock = Arc::new(Clock::default());
        let scheduler = Scheduler::new(Arc::clone(&clock));
        let id = scheduler.after("a", 5);
        scheduler.advance(4);
        assert!(scheduler.poll("a").is_empty(), "not due yet");
        scheduler.advance(5);
        assert_eq!(scheduler.poll("a"), vec![id]);
        // A one-shot fires once.
        scheduler.advance(6);
        assert!(scheduler.poll("a").is_empty());
    }

    #[test]
    fn a_repeating_timer_fires_every_period() {
        let clock = Arc::new(Clock::default());
        let scheduler = Scheduler::new(clock);
        let id = scheduler.every("a", 3);
        scheduler.advance(3);
        assert_eq!(scheduler.poll("a"), vec![id]);
        // Skipping several periods still yields one delivery this poll.
        scheduler.advance(30);
        assert_eq!(scheduler.poll("a"), vec![id]);
        scheduler.advance(33);
        assert_eq!(scheduler.poll("a"), vec![id]);
    }

    #[test]
    fn a_timer_is_owned_by_its_caller() {
        let clock = Arc::new(Clock::default());
        let scheduler = Scheduler::new(clock);
        let a = scheduler.after("a", 1);
        scheduler.advance(1);
        // `b` cannot poll or cancel `a`'s timer.
        assert!(scheduler.poll("b").is_empty());
        assert!(!scheduler.cancel("b", a));
        assert_eq!(scheduler.poll("a"), vec![a]);
    }

    #[test]
    fn a_cancelled_timer_never_fires() {
        let clock = Arc::new(Clock::default());
        let scheduler = Scheduler::new(clock);
        let id = scheduler.after("a", 2);
        assert!(scheduler.cancel("a", id));
        scheduler.advance(2);
        assert!(scheduler.poll("a").is_empty());
    }

    #[test]
    fn timers_fire_in_due_order_and_only_once_due() {
        // Several timers with different due ticks: an advance to a later tick
        // delivers exactly the ones now due, regardless of scheduling order.
        let clock = Arc::new(Clock::default());
        let scheduler = Scheduler::new(clock);
        let late = scheduler.after("a", 30);
        let soon = scheduler.after("a", 5);
        let mid = scheduler.after("a", 10);
        scheduler.advance(9);
        assert_eq!(scheduler.poll("a"), vec![soon]);
        scheduler.advance(30);
        assert_eq!(scheduler.poll("a"), vec![mid, late]);
        // Nothing left.
        scheduler.advance(100);
        assert!(scheduler.poll("a").is_empty());
    }

    #[test]
    fn an_idle_advance_does_no_work_and_delivers_nothing() {
        // `advance` peeks the heap; with no timers it is a no-op and nothing is
        // ever delivered, even across many ticks.
        let clock = Arc::new(Clock::default());
        let scheduler = Scheduler::new(clock);
        for n in 0..1000 {
            scheduler.advance(n);
            assert!(scheduler.poll("a").is_empty());
        }
    }

    #[test]
    fn a_cooldown_is_not_ready_until_it_expires() {
        let clock = Arc::new(Clock::default());
        let cooldowns = Cooldowns::new(Arc::clone(&clock));
        assert!(cooldowns.ready("a", "home", 20));
        assert_eq!(cooldowns.remaining("a", "home"), 20);
        clock.set(10);
        assert!(!cooldowns.ready("a", "home", 20));
        assert_eq!(cooldowns.remaining("a", "home"), 10);
        clock.set(20);
        assert!(cooldowns.ready("a", "home", 20));
        // Another caller's cooldown is independent.
        assert!(cooldowns.ready("b", "home", 20));
    }

    #[test]
    fn a_confirmation_must_be_requested_then_checked() {
        let clock = Arc::new(Clock::default());
        let backend: Arc<OnceLock<Arc<dyn HostBackend>>> = Arc::new(OnceLock::new());
        let confirm = Confirm::new(Arc::clone(&clock), Arc::new(Messages::new(backend)));
        let p = Player {
            id: 1,
            name: "Steve".to_string(),
            operator: false,
        };
        assert!(confirm.request("a", &p, "really?", "delete"));
        // A second request for the same player is refused while one is pending.
        assert!(!confirm.request("a", &p, "really?", "other"));
        assert!(confirm.check("a", &p, "delete"));
        // Consumed: a second check fails.
        assert!(!confirm.check("a", &p, "delete"));
    }

    #[test]
    fn an_expired_confirmation_is_not_accepted() {
        let clock = Arc::new(Clock::default());
        let backend: Arc<OnceLock<Arc<dyn HostBackend>>> = Arc::new(OnceLock::new());
        let confirm = Confirm::new(Arc::clone(&clock), Arc::new(Messages::new(backend)));
        let p = Player {
            id: 1,
            name: "Steve".to_string(),
            operator: false,
        };
        confirm.request("a", &p, "really?", "delete");
        clock.set(CONFIRM_TICKS + 1);
        assert!(!confirm.check("a", &p, "delete"));
    }
}

#[cfg(test)]
mod cost {
    //! Allocation tests for the hot paths. A thread-local counter, touched only
    //! by the measuring thread, so parallel tests do not interfere.
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
    }

    struct Counting;

    // SAFETY: delegates every allocation to `System`, only incrementing a
    // thread-local counter first; the counter uses `try_with`, so it never
    // panics (including during thread teardown).
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            ALLOCS.try_with(|c| c.set(c.get() + 1)).ok();
            // SAFETY: `l` is the layout the caller passed, forwarded unchanged.
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            // SAFETY: `p`/`l` are the pointer and layout the caller gave back.
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            ALLOCS.try_with(|c| c.set(c.get() + 1)).ok();
            // SAFETY: as `alloc`, forwarded unchanged.
            unsafe { System.alloc_zeroed(l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            ALLOCS.try_with(|c| c.set(c.get() + 1)).ok();
            // SAFETY: `p`/`l` describe the existing block, `n` the new size; all
            // forwarded unchanged to `System`.
            unsafe { System.realloc(p, l, n) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    /// Run `f`, returning how many allocations the current thread made.
    fn measure<R>(f: impl FnOnce() -> R) -> (R, usize) {
        ALLOCS.with(|c| c.set(0));
        let r = f();
        let n = ALLOCS.with(|c| c.get());
        (r, n)
    }

    #[test]
    fn an_idle_scheduler_advance_does_not_allocate() {
        let clock = Arc::new(Clock::default());
        let scheduler = Scheduler::new(clock);
        scheduler.advance(0); // warm any lazy init
        let (_, allocs) = measure(|| {
            for n in 1..=1000 {
                scheduler.advance(n);
                let _ = scheduler.poll("a");
            }
        });
        assert_eq!(allocs, 0, "an idle advance and poll must not allocate");
    }

    #[test]
    fn hot_queries_do_not_allocate() {
        let clock = Arc::new(Clock::default());
        let cooldowns = Cooldowns::new(clock);
        let commands = Commands::default();
        let (_, allocs) = measure(|| {
            for _ in 0..1000 {
                // A cooldown that is not set: a pure read.
                let _ = cooldowns.remaining("a", "home");
                // An unknown command: an empty resolution.
                let _ = commands.resolve("a", "nope", &[]);
            }
        });
        assert_eq!(allocs, 0, "unknown-key reads must not allocate");
    }
}

#[cfg(test)]
mod placeholder_tests {
    //! The shared placeholder runtime: built-ins, registration, pushed values
    //! and the resolved-token cache.

    use super::*;
    use aether_plugin::placeholders::PlayerExtra;

    /// A backend with a fixed per-player view, so the built-ins resolve to
    /// known values.
    struct ViewBackend;

    impl HostBackend for ViewBackend {
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
        fn online_players(&self, _: &str) -> Vec<Player> {
            vec![who()]
        }
        fn player_extra(&self, _: &str, _: u64) -> Option<PlayerExtra> {
            Some(PlayerExtra {
                ping: 42,
                world: "minecraft:overworld".to_string(),
                x: 10.4,
                y: 64.0,
                z: -3.9,
                health: 19.5,
                level: 5,
                display_name: "Steve".to_string(),
            })
        }
        fn max_players(&self, _: &str) -> u32 {
            20
        }
        fn server_tps(&self, _: &str) -> f64 {
            19.9
        }
        fn world_time(&self, _: &str) -> i64 {
            6000
        }
    }

    fn who() -> Player {
        Player {
            id: 7,
            name: "Steve".to_string(),
            operator: false,
        }
    }

    fn placeholders() -> Placeholders {
        let p = Placeholders::new(Arc::new(OnceLock::new()));
        p.set_backend(Arc::new(ViewBackend));
        p.advance(1);
        p
    }

    #[test]
    fn builtins_resolve_from_the_host_view() {
        let p = placeholders();
        let q = who();
        assert_eq!(p.substitute("sdk", &q, "%player%"), "Steve");
        assert_eq!(p.substitute("sdk", &q, "%online%/%max%"), "1/20");
        assert_eq!(p.substitute("sdk", &q, "%ping%"), "42");
        assert_eq!(p.substitute("sdk", &q, "%x% %y% %z%"), "10 64 -4");
        assert_eq!(p.substitute("sdk", &q, "%tps%"), "19.9");
        assert_eq!(p.substitute("sdk", &q, "%time%"), "12:00", "world clock");
    }

    #[test]
    fn an_unknown_token_is_left_literal() {
        let p = placeholders();
        assert_eq!(p.substitute("sdk", &who(), "hi %nope%"), "hi %nope%");
        assert_eq!(p.substitute("sdk", &who(), "100%"), "100%");
        // No `%` at all is returned unchanged.
        assert_eq!(p.substitute("sdk", &who(), "plain"), "plain");
    }

    #[test]
    fn a_native_provider_overrides_a_builtin() {
        let p = placeholders();
        p.register(
            "p",
            "player".to_string(),
            Arc::new(|q: &PlaceholderQuery| Some(format!("[{}]", q.name))),
        );
        assert_eq!(p.substitute("p", &who(), "%player%"), "[Steve]");
    }

    #[test]
    fn a_provider_that_declines_falls_back_to_the_builtin() {
        let p = placeholders();
        p.register("p", "online".to_string(), Arc::new(|_| None));
        assert_eq!(p.substitute("p", &who(), "%online%"), "1");
    }

    #[test]
    fn a_pushed_value_resolves_and_updates_immediately() {
        let p = placeholders();
        p.set_value("wasm", "balance".to_string(), 7, "0".to_string());
        assert_eq!(p.substitute("wasm", &who(), "%balance%"), "0");
        p.set_value("wasm", "balance".to_string(), 7, "10".to_string());
        assert_eq!(p.substitute("wasm", &who(), "%balance%"), "10");
        // Another player's value is independent.
        assert_eq!(
            p.substitute("wasm", &Player { id: 8, ..who() }, "%balance%"),
            "%balance%"
        );
    }

    #[test]
    fn a_second_caller_cannot_hijack_anothers_name() {
        let p = placeholders();
        // `economy` owns `%balance%`.
        assert!(p.register_name("economy", "balance".to_string()));
        p.set_value("economy", "balance".to_string(), 7, "100".to_string());
        // An untrusted `evil` cannot claim or overwrite it.
        assert!(!p.register_name("evil", "balance".to_string()), "owned");
        p.set_value("evil", "balance".to_string(), 7, "0".to_string());
        assert_eq!(
            p.substitute("sidebar", &who(), "%balance%"),
            "100",
            "the owner's value is untouched"
        );
        // A native closure from another caller is refused too.
        assert!(!p.register(
            "evil",
            "balance".to_string(),
            Arc::new(|_| Some("0".into()))
        ));
        assert_eq!(p.substitute("sidebar", &who(), "%balance%"), "100");
    }

    #[test]
    fn a_plugin_cannot_shadow_a_builtin_by_pushing() {
        let p = placeholders();
        assert!(
            !p.register_name("evil", "player".to_string()),
            "built-ins are reserved"
        );
        p.set_value("evil", "player".to_string(), 7, "Notch".to_string());
        assert_eq!(
            p.substitute("evil", &who(), "%player%"),
            "Steve",
            "the pushed built-in is ignored"
        );
    }

    #[test]
    fn the_same_caller_may_update_its_own_pushed_name() {
        let p = placeholders();
        assert!(p.register_name("economy", "balance".to_string()));
        p.set_value("economy", "balance".to_string(), 7, "1".to_string());
        p.set_value("economy", "balance".to_string(), 7, "2".to_string());
        assert_eq!(p.substitute("sidebar", &who(), "%balance%"), "2");
    }

    #[test]
    fn a_resolved_token_is_cached_for_the_ttl() {
        let p = placeholders();
        let calls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&calls);
        p.register(
            "p",
            "hit".to_string(),
            Arc::new(move |_q: &PlaceholderQuery| {
                counter.fetch_add(1, Ordering::Relaxed);
                Some("X".to_string())
            }),
        );
        p.advance(1);
        assert_eq!(p.substitute("p", &who(), "%hit%"), "X");
        assert_eq!(p.substitute("p", &who(), "%hit%"), "X");
        assert_eq!(calls.load(Ordering::Relaxed), 1, "the second hit cached");
        // Past the 20-tick ttl the provider is consulted again.
        p.advance(22);
        assert_eq!(p.substitute("p", &who(), "%hit%"), "X");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_new_provider_is_visible_immediately_despite_the_cache() {
        let p = placeholders();
        assert_eq!(p.substitute("p", &who(), "%late%"), "%late%");
        p.register(
            "p",
            "late".to_string(),
            Arc::new(|_| Some("now".to_string())),
        );
        assert_eq!(p.substitute("p", &who(), "%late%"), "now");
    }

    #[test]
    fn the_query_is_rebuilt_each_tick() {
        // A per-tick fact is not frozen by the query memo: a new tick rebuilds.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let p = placeholders();
        p.register(
            "p",
            "tick".to_string(),
            Arc::new(move |_q: &PlaceholderQuery| {
                sink.lock().unwrap().push(());
                Some("t".to_string())
            }),
        );
        p.advance(1);
        let _ = p.substitute("p", &who(), "%tick%");
        p.advance(100);
        let _ = p.substitute("p", &who(), "%tick%");
        // The provider is called twice: the ttl expired across the ticks.
        assert_eq!(seen.lock().unwrap().len(), 2);
    }
}
