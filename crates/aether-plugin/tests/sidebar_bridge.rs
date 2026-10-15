//! The sidebar bridge, end to end: a real WASM dependent
//! (`tests/fixtures/sidebar/sidebar.wasm`) calls `aether:sidebar` imports and
//! the host forwards them to the registered provider.
//!
//! This is the integration the unit tests cannot reach: it proves a dependent's
//! `sidebar.register-placeholder`, `set-placeholder`, `set-line`, `toggle` (and
//! friends) cross the component boundary once and land on the provider with the
//! caller's id attached. The provider here is a mock (the real one is
//! `plugins/sidebar`), but the path through the host linker and the bridge is
//! the same.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aether_plugin::sidebar::{self, PlaceholderFn, SidebarService};
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
    fn log(&self, _: &str, _: Level, _: &str) {}
    fn config(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn supports(&self, _: &str, _: &str) -> bool {
        false
    }
}
use LogLevel as Level;

/// Records every sidebar call the bridge forwards.
#[derive(Default)]
struct Recorder {
    declared: Mutex<Vec<(String, String)>>,
    values: Mutex<Vec<(String, String, u64, String)>>,
    lines: Mutex<Vec<(u64, usize, String)>>,
    cleared: Mutex<Vec<(u64, usize)>>,
    shown: Mutex<Vec<u64>>,
    hidden: Mutex<Vec<u64>>,
    toggled: Mutex<Vec<u64>>,
}

impl SidebarService for Recorder {
    fn register_placeholder(&self, _name: String, _provider: PlaceholderFn) -> bool {
        false
    }
    fn register_placeholder_name(&self, caller: &str, name: String) -> bool {
        self.declared
            .lock()
            .unwrap()
            .push((caller.to_string(), name));
        false
    }
    fn set_placeholder_value(&self, caller: &str, name: String, player: u64, value: String) {
        self.values
            .lock()
            .unwrap()
            .push((caller.to_string(), name, player, value));
    }
    fn set_line(&self, player: u64, index: usize, text: String) {
        self.lines.lock().unwrap().push((player, index, text));
    }
    fn clear_line(&self, player: u64, index: usize) {
        self.cleared.lock().unwrap().push((player, index));
    }
    fn show(&self, player: u64) {
        self.shown.lock().unwrap().push(player);
    }
    fn hide(&self, player: u64) {
        self.hidden.lock().unwrap().push(player);
    }
    fn toggle(&self, player: u64) -> bool {
        self.toggled.lock().unwrap().push(player);
        true
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
fn a_wasm_dependent_drives_the_sidebar_through_the_host() {
    let backend = Arc::new(Backend);
    let mut h = PluginHost::new(HostConfig::default(), backend).unwrap();

    let recorder = Arc::new(Recorder::default());
    sidebar::register_sidebar(h.services(), recorder.clone()).unwrap();

    let m = aether_plugin::manifest::load(&fixtures().join("sidebar/plugin.toml")).unwrap();
    h.add(&m, &["log".to_string()], &Default::default())
        .unwrap();
    assert_eq!(h.ids(), vec!["sid".to_string()]);

    let result = h.on_command(&player(), "sid", &[]).unwrap();
    assert!(result.handled);
    assert_eq!(result.lines, vec!["shown=true replaced=false".to_string()]);

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
        recorder.lines.lock().unwrap().as_slice(),
        [(42, 3, "line three".to_string())]
    );
    assert_eq!(recorder.cleared.lock().unwrap().as_slice(), [(42, 3)]);
    assert_eq!(recorder.toggled.lock().unwrap().as_slice(), [42]);
    assert_eq!(recorder.shown.lock().unwrap().as_slice(), [42]);
    assert_eq!(recorder.hidden.lock().unwrap().as_slice(), [42]);
}

#[test]
fn the_bridge_is_neutral_when_no_sidebar_provider_is_registered() {
    // No provider: the import is still satisfied by the linker (so the plugin
    // loads), but every call is refused and answers the neutral value.
    let backend = Arc::new(Backend);
    let mut h = PluginHost::new(HostConfig::default(), backend).unwrap();
    let m = aether_plugin::manifest::load(&fixtures().join("sidebar/plugin.toml")).unwrap();
    h.add(&m, &["log".to_string()], &Default::default())
        .unwrap();
    let result = h.on_command(&player(), "sid", &[]).unwrap();
    assert!(result.handled);
    assert_eq!(result.lines, vec!["shown=false replaced=false".to_string()]);
}
