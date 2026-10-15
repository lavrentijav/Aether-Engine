//! The Aether plugin SDK, as a **native plugin**.
//!
//! This is the runtime half of the SDK: it runs in-process and publishes its
//! helpers as services (`sdk.permissions@0.1`, `sdk.commands@0.1`, …) through
//! the host's service registry. A dependent plugin — native or WASM — declares
//! the dependency and calls these with one function call; the host forwards a
//! WASM caller's import to [`SdkPlugin`] in-process.
//!
//! The SDK is deliberately **thin**: it holds state the helpers genuinely need
//! (the command registry now; the scheduler, cooldowns and confirmations in
//! later steps) and otherwise fronts core mechanisms. It never implements the
//! game itself. The permission *model* is core (CMD-01/02); until that lands
//! the permission service fails closed, so a dependent sees deny rather than a
//! wrong answer, exactly as the design requires.
//!
//! It is a plugin like any other: the server adds it through the build-time
//! native registry, its manifest can disable it, and a dependent that
//! `requires` one of its services refuses to load when no provider is present.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aether_plugin::manifest::{Manifest, PluginTier};
use aether_plugin::sdk::{self, ArgKind, CommandSpec, CommandsService, PermissionsService};
use aether_plugin::{NativeContext, NativePlugin, NativeRegistration, Player};

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
#[derive(Default)]
pub struct SdkPlugin {
    commands: Arc<Commands>,
}

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

    /// Pair each declared argument with the supplied value, validating the
    /// scalar kinds. Extras beyond the spec and a missing required argument are
    /// reported by the caller through the usage line; here we return what
    /// parsed.
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

/// Whether `value` is a plausible value for an argument of `kind`. Only the
/// scalar kinds are checked here; `word`/`text` accept anything, `player`/`item`
/// are validated against the world when the command actually runs.
fn value_is_valid(kind: ArgKind, value: &str) -> bool {
    match kind {
        ArgKind::Integer => value.parse::<i64>().is_ok(),
        ArgKind::Word | ArgKind::Player | ArgKind::Item | ArgKind::Text => !value.is_empty(),
    }
}

// --- permission service ------------------------------------------------

/// Permission checks, fronting the core resolver.
///
/// The core permission model (CMD-01/02) is not available on this branch, so
/// every check denies and every write refuses with a clear message. This is the
/// approved "fail closed until the core lands" behaviour: a dependent is never
/// told it may do something it may not.
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

    fn grant(
        &self,
        _caller: &str,
        _subject: &str,
        _node: &str,
        _value: bool,
    ) -> Result<(), String> {
        Err("the permission model (CMD-01/02) is not available yet".to_string())
    }

    fn revoke(&self, _caller: &str, _subject: &str, _node: &str) -> Result<(), String> {
        Err("the permission model (CMD-01/02) is not available yet".to_string())
    }
}

// --- command service ---------------------------------------------------

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

// --- the plugin --------------------------------------------------------

impl NativePlugin for SdkPlugin {
    fn init(&mut self, ctx: &mut NativeContext) -> Result<(), String> {
        let commands: Arc<dyn CommandsService> = self.commands.clone();
        sdk::register_commands(ctx.services, commands)?;
        let permissions: Arc<dyn PermissionsService> = Arc::new(Permissions);
        sdk::register_permissions(ctx.services, permissions)?;
        ctx.log(
            aether_plugin::LogLevel::Info,
            "SDK services registered (sdk.permissions@0.1, sdk.commands@0.1)",
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_plugin::ServiceRegistry;

    fn spec(name: &str, usage: &str, args: Vec<(&str, ArgKind)>) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            parent: None,
            permission: None,
            usage: usage.to_string(),
            description: String::new(),
            args: args
                .into_iter()
                .map(|(n, kind)| aether_plugin::sdk::Arg {
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
        // Another plugin cannot see or parse `a`'s command.
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
        // Even an operator is denied while the core model is absent: fail
        // closed, never allow by accident.
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
    fn init_registers_both_services() {
        let registry = ServiceRegistry::new();
        let ctx_services = &registry;
        // Drive the registration helpers directly, as `init` does through the
        // context, so the test needs no full host.
        let commands: Arc<dyn CommandsService> = Arc::new(Commands::default());
        sdk::register_commands(ctx_services, commands).unwrap();
        sdk::register_permissions(ctx_services, Arc::new(Permissions)).unwrap();
        assert!(sdk::commands(ctx_services).is_some());
        assert!(sdk::permissions(ctx_services).is_some());
    }
}
