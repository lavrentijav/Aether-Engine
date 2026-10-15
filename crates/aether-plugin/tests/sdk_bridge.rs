//! The SDK bridge, end to end: a real WASM dependent (`greet`, built from
//! `plugins/examples/greet`) calls `aether:sdk` imports, and the host forwards
//! them to whichever provider is registered.
//!
//! This is the integration the unit tests cannot reach: it proves a dependent
//! component's `commands.register` and `messages.send` cross the component
//! boundary once and land on the provider, with the caller's id attached. The
//! providers here are mocks, not the real `plugins/sdk`, but the path through
//! the host linker and the bridge is the same.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aether_plugin::sdk::{
    self, CommandSpec, CommandsService, MessagesService, PlayersService, Pos, TeleportCause,
    TitleTimes,
};
use aether_plugin::{HostBackend, HostConfig, LogLevel, Player, PluginHost};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A backend that only needs to satisfy logging.
#[derive(Default)]
struct Backend {
    logs: Mutex<Vec<String>>,
}

impl HostBackend for Backend {
    fn kv_get(&self, _: &str, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn kv_put(&self, _: &str, _: &str, _: &[u8]) {}
    fn kv_delete(&self, _: &str, _: &str) {}
    fn kv_list(&self, _: &str, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn log(&self, plugin: &str, _: LogLevel, message: &str) {
        self.logs
            .lock()
            .unwrap()
            .push(format!("{plugin}: {message}"));
    }
    fn config(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn supports(&self, _: &str, _: &str) -> bool {
        false
    }
}

/// Records what a dependent registers and sends.
#[derive(Default)]
struct Recorder {
    registered: Mutex<Vec<(String, String)>>,
    sent: Mutex<Vec<(String, u64, String)>>,
}

impl CommandsService for Recorder {
    fn register(&self, caller: &str, spec: CommandSpec) -> Result<(), String> {
        self.registered
            .lock()
            .unwrap()
            .push((caller.to_string(), spec.name.clone()));
        Ok(())
    }
    fn usage(&self, _: &str, _: &str) -> String {
        String::new()
    }
    fn parse(&self, _: &str, _: &str, _: &[String]) -> Vec<(String, String)> {
        Vec::new()
    }
}

impl MessagesService for Recorder {
    fn send(&self, caller: &str, p: &Player, text: &str) {
        self.sent
            .lock()
            .unwrap()
            .push((caller.to_string(), p.id, text.to_string()));
    }
    fn send_key(&self, _: &str, _: &Player, _: &str, _: &[(String, String)]) {}
    fn actionbar(&self, _: &str, _: &Player, _: &str) {}
    fn title(&self, _: &str, _: &Player, _: &str, _: Option<&str>, _: TitleTimes) {}
    fn broadcast(&self, _: &str, _: &str) {}
    fn prefix(&self, _: &str, _: &Player) -> String {
        String::new()
    }
}

/// A no-op player provider so `sdk.players@0.1` resolves if a test needs it.
struct NoPlayers;
impl PlayersService for NoPlayers {
    fn online(&self, _: &str) -> Vec<Player> {
        Vec::new()
    }
    fn by_name(&self, _: &str, _: &str) -> Option<Player> {
        None
    }
    fn by_id(&self, _: &str, _: u64) -> Option<Player> {
        None
    }
    fn offline_name(&self, _: &str, _: u64, _: u64) -> Option<String> {
        None
    }
    fn teleport(&self, _: &str, _: &Player, _: Pos, _: TeleportCause) -> Result<(), String> {
        Err("test".to_string())
    }
    fn give(&self, _: &str, _: &Player, _: &str, count: u32) -> u64 {
        count as u64
    }
}

fn player() -> Player {
    Player {
        id: 42,
        name: "Steve".to_string(),
        operator: false,
    }
}

#[test]
fn a_wasm_dependent_calls_the_sdk_through_the_host() {
    let backend = Arc::new(Backend::default());
    let mut h = PluginHost::new(HostConfig::default(), backend).unwrap();

    // Register the provider side, as the native SDK plugin's `init` would.
    let recorder = Arc::new(Recorder::default());
    sdk::register_commands(h.services(), recorder.clone()).unwrap();
    sdk::register_messages(h.services(), recorder.clone()).unwrap();
    sdk::register_players(h.services(), Arc::new(NoPlayers)).unwrap();

    // The dependent requires exactly these services; it loads now.
    let m = aether_plugin::manifest::load(&fixtures().join("greet/plugin.toml")).unwrap();
    h.add(&m, &["log".to_string()], &Default::default())
        .unwrap();
    assert_eq!(h.ids(), vec!["greet".to_string()]);

    // Its `init` registered `/greet` through the service, with its own id.
    assert_eq!(
        recorder.registered.lock().unwrap().as_slice(),
        [("greet".to_string(), "greet".to_string())]
    );

    // `/greet` reaches the messages service; nothing is returned inline. The
    // text is raw here because the mock provider stands in for the SDK plugin,
    // which is where colour translation happens (unit-tested there).
    let result = h.on_command(&player(), "greet", &[]).unwrap();
    assert!(result.handled);
    assert!(result.lines.is_empty());
    assert_eq!(
        recorder.sent.lock().unwrap().as_slice(),
        [("greet".to_string(), 42, "&aHello, &fSteve&a!".to_string())]
    );
}

#[test]
fn a_dependent_refuses_to_load_without_the_sdk() {
    // No providers registered: the `requires` check refuses the load, so the
    // dependent never half-runs.
    let backend = Arc::new(Backend::default());
    let mut h = PluginHost::new(HostConfig::default(), backend).unwrap();
    let m = aether_plugin::manifest::load(&fixtures().join("greet/plugin.toml")).unwrap();
    let err = h
        .add(&m, &["log".to_string()], &Default::default())
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("sdk.commands@0.1"), "{text}");
    assert!(text.contains("not registered"), "{text}");
    assert!(h.ids().is_empty());
}
