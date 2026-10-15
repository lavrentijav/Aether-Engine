//! End to end through the **real** `plugins/sdk` native plugin.
//!
//! `sdk_bridge.rs` in `aether-plugin` drives a WASM dependent to *mock*
//! providers. This test instead loads the actual `SdkPlugin` and the checked-in
//! `greet.wasm`, so the whole path — the guest's `commands.register` and
//! `messages.send`, the host linker, the real service implementations, and the
//! service-layer capability gate — runs for real.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aether_plugin::{HostBackend, HostConfig, LogLevel, ManifestFile, Player, PluginHost};

/// A backend that records chat lines and grants the caller `messages.send`.
struct Backend {
    sent: Mutex<Vec<(u64, String)>>,
    /// Commands the SDK registered with the core registry (recorded, not
    /// dispatched — this test does not build a server).
    registered: Mutex<Vec<String>>,
    /// The effective grant for `greet`, mirroring what the server records when
    /// it loads the plugin.
    grant: Vec<String>,
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
            self.grant.clone()
        } else {
            Vec::new()
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
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/aether-plugin/tests/fixtures/greet/plugin.toml")
}

#[test]
fn greet_runs_through_the_real_sdk_plugin() {
    let backend = Arc::new(Backend {
        sent: Mutex::new(Vec::new()),
        registered: Mutex::new(Vec::new()),
        grant: vec!["messages.send".to_string()],
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
        registered: Mutex::new(Vec::new()),
        grant: Vec::new(),
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
