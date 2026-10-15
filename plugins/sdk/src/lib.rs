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
//! implements the game itself. Two core mechanisms are not on this branch, so
//! their services report the gap rather than guessing: the permission model
//! (CMD-01/02) fails closed, and teleport/give (CMD-31/32) return the neutral
//! answer. That is the approved design: facades, not a second implementation.
//!
//! It is a plugin like any other: the server adds it through the build-time
//! native registry, its manifest can disable it, and a dependent that
//! `requires` one of its services refuses to load when no provider is present.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use aether_plugin::manifest::{Manifest, PluginTier};
use aether_plugin::sdk::{
    self, ArgKind, CommandSpec, CommandsService, ConfirmService, CooldownsService, MessagesService,
    PermissionsService, PlayersService, Pos, Resolution, SchedulerService, TeleportCause,
    TitleTimes,
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

/// The SDK service host.
pub struct SdkPlugin {
    commands: Arc<Commands>,
    messages: Arc<Messages>,
    players: Arc<Players>,
    scheduler: Arc<Scheduler>,
    cooldowns: Arc<Cooldowns>,
    confirm: Arc<Confirm>,
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
            players: Arc::new(Players::new(backend)),
        }
    }
}

impl NativePlugin for SdkPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let backend = Arc::clone(ctx.backend);
        self.commands.set_backend(Arc::clone(&backend));
        self.messages.set_backend(Arc::clone(&backend));
        self.players.set_backend(backend);

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

        ctx.log(
            LogLevel::Info,
            "SDK services registered (permissions, commands, messages, players, \
             scheduler, cooldowns, confirm)",
        );
        Ok(())
    }

    fn on_tick(&mut self, _ctx: &mut NativeContext, n: u64) {
        // Advance the clock the timer-driven services read, then fire due
        // timers into the pending queue a dependent polls.
        self.scheduler.advance(n);
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
    specs: Mutex<HashMap<(String, String), CommandSpec>>,
    /// Subcommands by `(caller, parent)` then subcommand name.
    subs: Mutex<HashMap<(String, String), HashMap<String, CommandSpec>>>,
    backend: Arc<OnceLock<Arc<dyn HostBackend>>>,
}

impl Commands {
    fn new(backend: Arc<OnceLock<Arc<dyn HostBackend>>>) -> Commands {
        Commands {
            specs: Mutex::new(HashMap::new()),
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
            let by_name = subs.entry((caller.to_string(), parent)).or_default();
            if by_name.contains_key(&spec.name) {
                return Err(format!("subcommand `{}` is already registered", spec.name));
            }
            by_name.insert(spec.name.clone(), spec);
            return Ok(());
        }
        let key = (caller.to_string(), spec.name.clone());
        // Cheap duplicate check, then release the lock before the host call:
        // `register_command` reaches the core registry, and holding `specs`
        // across a host call would risk a lock-order inversion for no reason.
        if self.specs.lock().unwrap().contains_key(&key) {
            return Err(format!("command `{}` is already registered", spec.name));
        }
        // Register with the core registry first, so a core-name clash or a
        // duplicate is refused before a spec is stored. No lock is held here.
        let node = command_node(caller, &spec);
        if let Some(backend) = self.backend.get() {
            backend.register_command(caller, &spec.name, &node, spec.op)?;
        }
        let mut specs = self.specs.lock().unwrap();
        // Re-check under the lock: a concurrent register that slipped past the
        // fast check above must not be silently overwritten.
        if specs.contains_key(&key) {
            return Err(format!("command `{}` is already registered", spec.name));
        }
        specs.insert(key, spec);
        Ok(())
    }

    fn get(&self, caller: &str, name: &str) -> Option<CommandSpec> {
        self.specs
            .lock()
            .unwrap()
            .get(&(caller.to_string(), name.to_string()))
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
                .get(&(caller.to_string(), name.to_string()))
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
/// The core permission model (CMD-01/02) is not available on this branch, so
/// every check denies and every write refuses with a clear message. This is the
/// approved "fail closed until the core lands" behaviour. The admin write path
/// is gated on `permissions.admin` here — the one gate, applied by caller id,
/// so a native dependent is checked too.
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
    fn check(&self, _caller: &str, _p: &Player, _node: &str) -> bool {
        false
    }
    fn require(&self, _caller: &str, _p: &Player, _node: &str) -> bool {
        false
    }
    fn level(&self, _caller: &str, _p: &Player) -> u8 {
        0
    }
    fn groups(&self, _caller: &str, _p: &Player) -> Vec<String> {
        Vec::new()
    }
    fn meta(&self, _caller: &str, _p: &Player, _key: &str) -> Option<String> {
        None
    }
    fn grant(&self, caller: &str, _s: &str, _n: &str, _v: bool) -> Result<(), String> {
        if !self.may(caller, CAP_PERMISSIONS_ADMIN) {
            return Err(format!(
                "caller `{caller}` lacks the `{CAP_PERMISSIONS_ADMIN}` capability"
            ));
        }
        Err("the permission model (CMD-01/02) is not available yet".to_string())
    }
    fn revoke(&self, caller: &str, _s: &str, _n: &str) -> Result<(), String> {
        if !self.may(caller, CAP_PERMISSIONS_ADMIN) {
            return Err(format!(
                "caller `{caller}` lacks the `{CAP_PERMISSIONS_ADMIN}` capability"
            ));
        }
        Err("the permission model (CMD-01/02) is not available yet".to_string())
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

    fn prefix(&self, _caller: &str, _p: &Player) -> String {
        // Prefixes come from permission meta (CMD-02), absent here.
        String::new()
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
        _caller: &str,
        _p: &Player,
        _to: Pos,
        _cause: TeleportCause,
    ) -> Result<(), String> {
        Err("teleport (CMD-31) is not available yet".to_string())
    }

    fn give(&self, _caller: &str, _p: &Player, _item: &str, count: u32) -> u64 {
        // Nothing could be given, so all of it is "left over".
        count as u64
    }
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
struct Cooldowns {
    clock: Arc<Clock>,
    map: Mutex<HashMap<(String, String), (u64, u64)>>,
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
        let entry = map.get_mut(&(caller.to_string(), key.to_string()));
        match entry {
            Some((armed, duration)) if now < *armed + *duration => false,
            _ => {
                map.insert((caller.to_string(), key.to_string()), (now, ticks.max(1)));
                true
            }
        }
    }

    fn remaining(&self, caller: &str, key: &str) -> u64 {
        let now = self.clock.now();
        let map = self.map.lock().unwrap();
        match map.get(&(caller.to_string(), key.to_string())) {
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
    fn the_permission_service_fails_closed() {
        // Even an admin-capable caller is refused while the model is absent.
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
        // The admin write path is gated on the capability, and fails closed on
        // the missing model even when it is granted.
        assert!(p.grant("greet", "Steve", "n", true).is_err());
        assert!(p.revoke("greet", "Steve", "n").is_err());
    }

    #[test]
    fn the_permission_admin_gate_is_applied_in_the_service_layer() {
        // A caller lacking `permissions.admin` is refused with a capability
        // reason, before the missing-model reason.
        let p = Permissions::new(caps_backend("greet", &[]));
        let err = p.grant("greet", "Steve", "n", true).unwrap_err();
        assert!(err.contains("permissions.admin"), "{err}");
        // A caller that has it gets the missing-model reason instead.
        let p = Permissions::new(caps_backend("greet", &[CAP_PERMISSIONS_ADMIN]));
        let err = p.grant("greet", "Steve", "n", true).unwrap_err();
        assert!(err.contains("CMD-01/02"), "{err}");
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
        assert!(sdk::commands(&registry).is_some());
        assert!(sdk::permissions(&registry).is_some());
        assert!(sdk::messages(&registry).is_some());
        assert!(sdk::players(&registry).is_some());
        assert!(sdk::scheduler(&registry).is_some());
        assert!(sdk::cooldowns(&registry).is_some());
        assert!(sdk::confirm(&registry).is_some());
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
