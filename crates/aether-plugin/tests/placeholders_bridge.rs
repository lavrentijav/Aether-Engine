//! The shared placeholders bridge, end to end: a real WASM dependent
//! (`tests/fixtures/sidebar/sidebar.wasm`) calls the `aether:sdk/placeholders`
//! imports and the host forwards them to the registered provider.
//!
//! It proves a component's `register`, `set` and `resolve` cross the boundary
//! once and land on the [`PlaceholdersService`] with the caller's id attached —
//! the path a WASM economy/rank plugin uses to feed `%economy_balance%`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aether_plugin::placeholders::{self, PlaceholderFn, PlaceholdersService};
use aether_plugin::{HostBackend, HostConfig, LogLevel, Player, PluginHost};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[derive(Default)]
struct Backend;

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
}

/// Records every placeholders call the bridge forwards.
#[derive(Default)]
struct Recorder {
    declared: Mutex<Vec<(String, String)>>,
    values: Mutex<Vec<(String, String, u64, String)>>,
    resolved: Mutex<Vec<(String, u64, String)>>,
}

impl PlaceholdersService for Recorder {
    fn register(&self, _caller: &str, _name: String, _provider: PlaceholderFn) -> bool {
        false
    }
    fn register_name(&self, caller: &str, name: String) -> bool {
        self.declared
            .lock()
            .unwrap()
            .push((caller.to_string(), name));
        false
    }
    fn set_value(&self, caller: &str, name: String, player: u64, value: String) {
        self.values
            .lock()
            .unwrap()
            .push((caller.to_string(), name, player, value));
    }
    fn resolve(&self, caller: &str, player: &Player, text: &str) -> String {
        self.resolved
            .lock()
            .unwrap()
            .push((caller.to_string(), player.id, text.to_string()));
        // A toy substitution so the guest can see a non-literal result.
        text.replace("%balance%", "1234")
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
fn a_wasm_dependent_drives_the_placeholder_registry_through_the_host() {
    let backend = Arc::new(Backend);
    let mut h = PluginHost::new(HostConfig::default(), backend).unwrap();

    let recorder = Arc::new(Recorder::default());
    placeholders::register_placeholders(h.services(), recorder.clone()).unwrap();

    let m = aether_plugin::manifest::load(&fixtures().join("sidebar/plugin.toml")).unwrap();
    h.add(&m, &["log".to_string()], &Default::default())
        .unwrap();
    assert_eq!(h.ids(), vec!["sid".to_string()]);

    let result = h.on_command(&player(), "sid", &[]).unwrap();
    assert!(result.handled);
    // `resolve` came back through the host and the guest echoed it. The sidebar
    // provider is absent here, so `shown=false`.
    assert_eq!(
        result.lines,
        vec!["shown=false replaced=false resolved=b=1234".to_string()]
    );

    assert_eq!(
        recorder.declared.lock().unwrap().as_slice(),
        [("sid".to_string(), "balance".to_string())]
    );
    assert_eq!(
        recorder.values.lock().unwrap().as_slice(),
        [(
            "sid".to_string(),
            "balance".to_string(),
            42,
            "1234".to_string()
        )]
    );
    assert_eq!(
        recorder.resolved.lock().unwrap().as_slice(),
        [("sid".to_string(), 42, "b=%balance%".to_string())]
    );
}

#[test]
fn the_placeholders_bridge_is_neutral_when_no_provider_is_registered() {
    let backend = Arc::new(Backend);
    let mut h = PluginHost::new(HostConfig::default(), backend).unwrap();
    let m = aether_plugin::manifest::load(&fixtures().join("sidebar/plugin.toml")).unwrap();
    h.add(&m, &["log".to_string()], &Default::default())
        .unwrap();
    let result = h.on_command(&player(), "sid", &[]).unwrap();
    assert!(result.handled);
    // With no provider, `register` is refused (`false`) and `resolve` returns
    // its input unchanged.
    assert_eq!(
        result.lines,
        vec!["shown=false replaced=false resolved=b=%balance%".to_string()]
    );
}
