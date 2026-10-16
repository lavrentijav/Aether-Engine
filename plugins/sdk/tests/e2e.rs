//! End to end through the **real** `plugins/sdk` native plugin.
//!
//! `sdk_bridge.rs` in `aether-plugin` drives a WASM dependent to *mock*
//! providers. This test instead loads the actual `SdkPlugin` and the checked-in
//! `greet.wasm`, so the whole path — the guest's `commands.register` and
//! `messages.send`, the host linker, the real service implementations, and the
//! service-layer capability gate — runs for real.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aether_plugin::sdk::{HistoryEvent, HistoryPage, HistoryPos};
use aether_plugin::{HostBackend, HostConfig, LogLevel, ManifestFile, Player, PluginHost};

/// A backend that records chat lines and logs, and grants each caller the
/// capabilities its test configured.
struct Backend {
    sent: Mutex<Vec<(u64, String)>>,
    /// Log lines the SDK plugin emitted (used by the history probe).
    logs: Mutex<Vec<String>>,
    /// Commands the SDK registered with the core registry (recorded, not
    /// dispatched — this test does not build a server).
    registered: Mutex<Vec<String>>,
    /// The effective grant for `greet`, mirroring what the server records when
    /// it loads the plugin.
    grant: Vec<String>,
    /// The effective grant for `historian`.
    historian_grant: Vec<String>,
    /// Whether `history.worldgen_original` answers "original" when reached.
    worldgen_original: bool,
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
    fn send_message(&self, _plugin: &str, player: u64, text: &str) {
        self.sent.lock().unwrap().push((player, text.to_string()));
    }
    fn capabilities(&self, plugin: &str) -> Vec<String> {
        match plugin {
            "greet" => self.grant.clone(),
            "historian" => self.historian_grant.clone(),
            _ => Vec::new(),
        }
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

    // Reached only when the caller passed the `history.read` gate.
    fn history_worldgen_original(&self, _plugin: &str, _x: i32, _y: i32, _z: i32) -> bool {
        self.worldgen_original
    }

    fn history_block(
        &self,
        _plugin: &str,
        _x: i32,
        _y: i32,
        _z: i32,
        _before: Option<u64>,
        _limit: u32,
    ) -> HistoryPage {
        HistoryPage {
            events: vec![HistoryEvent {
                seq: 1,
                at_ms: 0,
                actor_low: 0,
                actor_high: 0,
                kind: "block_set".to_string(),
                pos: Some(HistoryPos { x: 1, y: 2, z: 3 }),
                from_block: None,
                to_block: None,
                uid: None,
                item: None,
                count: None,
                from_place: None,
                to_place: None,
                reason: None,
                cause: None,
                sources: Vec::new(),
            }],
            next: None,
        }
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/aether-plugin/tests/fixtures/greet/plugin.toml")
}

fn historian_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/aether-plugin/tests/fixtures/historian/plugin.toml")
}

#[test]
fn greet_runs_through_the_real_sdk_plugin() {
    let backend = Arc::new(Backend {
        sent: Mutex::new(Vec::new()),
        logs: Mutex::new(Vec::new()),
        registered: Mutex::new(Vec::new()),
        grant: vec!["messages.send".to_string()],
        historian_grant: Vec::new(),
        worldgen_original: false,
    });
    let mut host = PluginHost::new(HostConfig::default(), backend.clone()).unwrap();

    // Load the real SDK native plugin first (as the server does).
    let reg = aether_sdk_plugin::registration();
    let sdk = ManifestFile {
        dir: PathBuf::new(),
        manifest: reg.manifest,
    };
    host.add_native(&sdk, &[], &Default::default(), (reg.new)())
        .unwrap();

    // Then the WASM dependent. `greet` gets the `messages.send` capability.
    let greet = aether_plugin::manifest::load(&fixture()).unwrap();
    host.add(
        &greet,
        &["log".to_string(), "messages.send".to_string()],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(host.ids(), vec!["sdk".to_string(), "greet".to_string()]);

    // Its `init` registered `/greet` through the real command service.
    assert_eq!(backend.registered.lock().unwrap().as_slice(), ["greet"]);

    // `/greet` reaches the real message service, which colourises the text.
    let p = Player {
        id: 42,
        name: "Steve".to_string(),
        operator: false,
    };
    let result = host.on_command(&p, "greet", &[]).unwrap();
    assert!(result.handled);
    assert!(result.lines.is_empty(), "the reply goes through messages");
    assert_eq!(
        backend.sent.lock().unwrap().as_slice(),
        [(
            42u64,
            "\u{00a7}aHello, \u{00a7}fSteve\u{00a7}a!".to_string()
        )]
    );
}

#[test]
fn the_service_layer_gate_refuses_a_send_without_the_capability() {
    // Same wiring, but the caller is not granted `messages.send`; the real
    // message service refuses, so nothing is sent.
    let backend = Arc::new(Backend {
        sent: Mutex::new(Vec::new()),
        logs: Mutex::new(Vec::new()),
        registered: Mutex::new(Vec::new()),
        grant: Vec::new(),
        historian_grant: Vec::new(),
        worldgen_original: false,
    });
    let mut host = PluginHost::new(HostConfig::default(), backend.clone()).unwrap();
    let reg = aether_sdk_plugin::registration();
    host.add_native(
        &ManifestFile {
            dir: PathBuf::new(),
            manifest: reg.manifest,
        },
        &[],
        &Default::default(),
        (reg.new)(),
    )
    .unwrap();
    let greet = aether_plugin::manifest::load(&fixture()).unwrap();
    // No capabilities granted to greet.
    host.add(&greet, &["log".to_string()], &Default::default())
        .unwrap();

    let p = Player {
        id: 42,
        name: "Steve".to_string(),
        operator: false,
    };
    let result = host.on_command(&p, "greet", &[]).unwrap();
    assert!(result.handled);
    assert!(
        backend.sent.lock().unwrap().is_empty(),
        "a caller without `messages.send` must not send"
    );
}

/// A WASM dependent that calls the `history` service, through the real SDK
/// plugin, with and without the caller's `history.read` capability.
fn run_historian(historian_grant: Vec<String>) -> Vec<String> {
    let backend = Arc::new(Backend {
        sent: Mutex::new(Vec::new()),
        logs: Mutex::new(Vec::new()),
        registered: Mutex::new(Vec::new()),
        grant: Vec::new(),
        historian_grant,
        worldgen_original: true,
    });
    let mut host = PluginHost::new(HostConfig::default(), backend.clone()).unwrap();

    let reg = aether_sdk_plugin::registration();
    host.add_native(
        &ManifestFile {
            dir: PathBuf::new(),
            manifest: reg.manifest,
        },
        &[],
        &Default::default(),
        (reg.new)(),
    )
    .unwrap();

    let historian = aether_plugin::manifest::load(&historian_fixture()).unwrap();
    // The host grants the manifest's `log`; the history gate reads the
    // backend's per-caller grant instead.
    host.add(&historian, &["log".to_string()], &Default::default())
        .unwrap();

    let p = Player {
        id: 7,
        name: "Steve".to_string(),
        operator: false,
    };
    let result = host.on_command(&p, "hist", &[]).unwrap();
    assert!(result.handled);
    let logs = backend.logs.lock().unwrap().clone();
    logs
}

#[test]
fn a_wasm_dependent_reads_history_when_granted() {
    let logs = run_historian(vec!["log".to_string(), "history.read".to_string()]);
    assert!(
        logs.iter().any(|l| l.contains("historian orig=true n=1")),
        "a granted caller reaches the real backend: {logs:?}"
    );
}

#[test]
fn the_history_gate_refuses_a_dependent_without_history_read() {
    // The guest asks the same questions, but `history.read` was not granted:
    // the real provider refuses before forwarding, so the guest sees the
    // neutral answer (not original, no events).
    let logs = run_historian(vec!["log".to_string()]);
    assert!(
        logs.iter().any(|l| l.contains("historian orig=false n=0")),
        "an ungranted caller must be refused at the gate: {logs:?}"
    );
}
