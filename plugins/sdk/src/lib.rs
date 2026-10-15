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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use aether_plugin::manifest::{Manifest, PluginTier};
use aether_plugin::sdk::{
    self, ArgKind, CommandSpec, CommandsService, MessagesService, PermissionsService,
    PlayersService, Pos, TeleportCause, TitleTimes,
};
use aether_plugin::{
    HostBackend, LogLevel, NativeContext, NativePlugin, NativeRegistration, Player,
};

/// The SDK plugin's manifest. It subscribes to no events: it is a service host,
/// driven by the calls its dependents make.
pub fn manifest() -> Manifest {
    Manifest {
        id: "sdk".to_string(),
        version: "0.1.0".to_string(),
        api: "0.1".to_string(),
        entry: String::new(),
        name: "Aether SDK".to_string(),
        // Loads before gameplay plugins; the value only orders event calls,
        // which there are none of here.
        priority: 1000,
        capabilities: Vec::new(),
        subscribe: Vec::new(),
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

/// The SDK service host.
pub struct SdkPlugin {
    commands: Arc<Commands>,
    messages: Arc<Messages>,
    players: Arc<Players>,
}

impl Default for SdkPlugin {
    fn default() -> Self {
        // One shared slot the services read the host backend from, filled in
        // `init` before any dependent can call.
        let backend: Arc<OnceLock<Arc<dyn HostBackend>>> = Arc::new(OnceLock::new());
        SdkPlugin {
            commands: Arc::new(Commands::default()),
            messages: Arc::new(Messages::new(Arc::clone(&backend))),
            players: Arc::new(Players::new(backend)),
        }
    }
}

impl NativePlugin for SdkPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let backend = Arc::clone(ctx.backend);
        self.messages.set_backend(Arc::clone(&backend));
        self.players.set_backend(backend);

        let commands: Arc<dyn CommandsService> = self.commands.clone();
        sdk::register_commands(ctx.services, commands)?;
        let permissions: Arc<dyn PermissionsService> = Arc::new(Permissions);
        sdk::register_permissions(ctx.services, permissions)?;
        let messages: Arc<dyn MessagesService> = self.messages.clone();
        sdk::register_messages(ctx.services, messages)?;
        let players: Arc<dyn PlayersService> = self.players.clone();
        sdk::register_players(ctx.services, players)?;

        ctx.log(
            LogLevel::Info,
            "SDK services registered (permissions, commands, messages, players)",
        );
        Ok(())
    }
}

// --- commands ----------------------------------------------------------

/// The command registry the `sdk.commands` service fronts.
///
/// Keyed by `(caller, name)` so two dependents may each register their own
/// commands without colliding, and so a caller can only parse its own specs.
#[derive(Default)]
struct Commands {
    specs: Mutex<HashMap<(String, String), CommandSpec>>,
}

impl Commands {
    fn register(&self, caller: &str, spec: CommandSpec) -> Result<(), String> {
        if spec.name.is_empty() {
            return Err("command name must not be empty".to_string());
        }
        let mut specs = self.specs.lock().unwrap();
        let key = (caller.to_string(), spec.name.clone());
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

    fn parse(&self, caller: &str, name: &str, args: &[String]) -> Vec<(String, String)> {
        let Some(spec) = self.get(caller, name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (declared, value) in spec.args.iter().zip(args.iter()) {
            if !value_is_valid(declared.kind, value) {
                continue;
            }
            out.push((declared.name.clone(), value.clone()));
        }
        out
    }
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
/// approved "fail closed until the core lands" behaviour.
struct Permissions;

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
    fn grant(&self, _c: &str, _s: &str, _n: &str, _v: bool) -> Result<(), String> {
        Err("the permission model (CMD-01/02) is not available yet".to_string())
    }
    fn revoke(&self, _c: &str, _s: &str, _n: &str) -> Result<(), String> {
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
        self.deliver(p, text, caller);
    }

    fn send_key(&self, caller: &str, p: &Player, key: &str, args: &[(String, String)]) {
        let rendered = self.translate(key, args);
        self.deliver(p, &rendered, caller);
    }

    fn actionbar(&self, _caller: &str, _p: &Player, _text: &str) {
        // Action bars need the player/UI host API (CMD-32), absent here.
    }

    fn title(
        &self,
        _caller: &str,
        _p: &Player,
        _title: &str,
        _subtitle: Option<&str>,
        _times: TitleTimes,
    ) {
        // Titles need the player/UI host API (CMD-32), absent here.
    }

    fn broadcast(&self, caller: &str, text: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use aether_plugin::ServiceRegistry;
    use std::sync::Mutex as StdMutex;

    fn spec(name: &str, usage: &str, args: Vec<(&str, ArgKind)>) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            parent: None,
            permission: None,
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
    fn the_permission_service_fails_closed() {
        let p = Permissions;
        let player = Player {
            id: 1,
            name: "Steve".to_string(),
            operator: true,
        };
        assert!(!p.check("x", &player, "essentials.home"));
        assert!(!p.require("x", &player, "essentials.home"));
        assert_eq!(p.level("x", &player), 0);
        assert!(!p.has_level("x", &player, 2));
        assert!(p.groups("x", &player).is_empty());
        assert!(p.meta("x", &player, "prefix").is_none());
        assert!(p.grant("x", "Steve", "n", true).is_err());
        assert!(p.revoke("x", "Steve", "n").is_err());
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
        sdk::register_commands(&registry, Arc::new(Commands::default())).unwrap();
        sdk::register_permissions(&registry, Arc::new(Permissions)).unwrap();
        sdk::register_messages(&registry, Arc::new(Messages::new(Arc::clone(&backend)))).unwrap();
        sdk::register_players(&registry, Arc::new(Players::new(backend))).unwrap();
        assert!(sdk::commands(&registry).is_some());
        assert!(sdk::permissions(&registry).is_some());
        assert!(sdk::messages(&registry).is_some());
        assert!(sdk::players(&registry).is_some());
    }
}
