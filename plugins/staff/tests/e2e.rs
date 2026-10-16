//! The staff plugin driven through the real host trait with a recording
//! backend: what the core is asked to do is what these tests check.
//!
//! The core *mechanism* (the visibility predicate, the spectate snapshot, the
//! anti-xray override) is tested in `aether-server`'s `staff`/`visibility`
//! modules; here the backend is a stub, so these tests assert the plugin's
//! policy — gating, persistence, the auto-vanish and the x-ray gate.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use aether_plugin::{HostBackend, LogLevel, NativeContext, NativePlugin, Player, ServiceRegistry};

use aether_staff_plugin::registration;

/// A recording backend: it remembers the staff calls and the plugin KV.
#[derive(Default)]
struct Mock {
    players: Vec<Player>,
    vanished: Mutex<HashSet<u64>>,
    sees: Mutex<HashMap<u64, bool>>,
    spectating: Mutex<HashSet<u64>>,
    enters: Mutex<Vec<(u64, Option<u64>)>>,
    leaves: Mutex<Vec<u64>>,
    xray: Mutex<Vec<(u64, String)>>,
    actionbars: Mutex<Vec<(u64, String)>>,
    journal: Mutex<Vec<(u64, String, String)>>,
    kv: Mutex<HashMap<String, Vec<u8>>>,
    commands: Mutex<Vec<(String, String, u8)>>,
}

impl HostBackend for Mock {
    fn kv_get(&self, _plugin: &str, key: &str) -> Option<Vec<u8>> {
        self.kv.lock().unwrap().get(key).cloned()
    }
    fn kv_put(&self, _plugin: &str, key: &str, value: &[u8]) {
        self.kv
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_vec());
    }
    fn kv_delete(&self, _plugin: &str, key: &str) {
        self.kv.lock().unwrap().remove(key);
    }
    fn kv_list(&self, _plugin: &str, prefix: &str) -> Vec<String> {
        self.kv
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }
    fn log(&self, _plugin: &str, _level: LogLevel, _message: &str) {}
    fn config(&self, _plugin: &str, _key: &str) -> Option<String> {
        None
    }
    fn supports(&self, _plugin: &str, _feature: &str) -> bool {
        false
    }
    fn online_players(&self, _plugin: &str) -> Vec<Player> {
        self.players.clone()
    }
    fn register_command(
        &self,
        _plugin: &str,
        name: &str,
        node: &str,
        op: u8,
    ) -> Result<(), String> {
        self.commands
            .lock()
            .unwrap()
            .push((name.to_string(), node.to_string(), op));
        Ok(())
    }
    fn staff_set_vanished(&self, _plugin: &str, player: u64, vanished: bool) {
        let mut set = self.vanished.lock().unwrap();
        if vanished {
            set.insert(player);
        } else {
            set.remove(&player);
        }
    }
    fn staff_is_vanished(&self, _plugin: &str, player: u64) -> bool {
        self.vanished.lock().unwrap().contains(&player)
    }
    fn staff_set_sees_vanished(&self, _plugin: &str, player: u64, see: bool) {
        self.sees.lock().unwrap().insert(player, see);
    }
    fn staff_is_spectating(&self, _plugin: &str, player: u64) -> bool {
        self.spectating.lock().unwrap().contains(&player)
    }
    fn staff_enter_spectate(
        &self,
        _plugin: &str,
        player: u64,
        target: Option<u64>,
    ) -> Result<(), String> {
        self.enters.lock().unwrap().push((player, target));
        self.spectating.lock().unwrap().insert(player);
        self.vanished.lock().unwrap().insert(player);
        Ok(())
    }
    fn staff_leave_spectate(&self, _plugin: &str, player: u64) -> Result<(), String> {
        self.leaves.lock().unwrap().push(player);
        self.spectating.lock().unwrap().remove(&player);
        Ok(())
    }
    fn staff_set_xray_level(&self, _plugin: &str, player: u64, level: &str) -> Result<(), String> {
        self.xray.lock().unwrap().push((player, level.to_string()));
        Ok(())
    }
    fn staff_actionbar(&self, _plugin: &str, player: u64, text: &str) {
        self.actionbars
            .lock()
            .unwrap()
            .push((player, text.to_string()));
    }
    fn staff_journal(&self, _plugin: &str, actor: u64, action: &str, detail: &str) {
        self.journal
            .lock()
            .unwrap()
            .push((actor, action.to_string(), detail.to_string()));
    }
}

fn player(id: u64, name: &str, operator: bool) -> Player {
    Player {
        id,
        name: name.to_string(),
        operator,
    }
}

/// Build a plugin against a fresh backend with the one operator "Steve".
fn setup() -> (Box<dyn NativePlugin>, Arc<Mock>, Arc<ServiceRegistry>) {
    setup_with(vec![player(7, "Steve", true)], BTreeMap::new())
}

fn setup_with(
    players: Vec<Player>,
    config: BTreeMap<String, String>,
) -> (Box<dyn NativePlugin>, Arc<Mock>, Arc<ServiceRegistry>) {
    let backend = Arc::new(Mock {
        players,
        ..Mock::default()
    });
    let services = Arc::new(ServiceRegistry::new());
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut plugin = (registration().new)();
    let mut ctx = NativeContext {
        id: "staff",
        capabilities: &[],
        config: &config,
        services: &services,
        backend: &backend_dyn,
    };
    plugin.init(&mut ctx).expect("init");
    (plugin, backend, services)
}

/// A syntax helper: run `on_command` for player `p` on the plugin.
fn run(
    plugin: &mut Box<dyn NativePlugin>,
    backend: &Arc<Mock>,
    services: &Arc<ServiceRegistry>,
    p: &Player,
    name: &str,
    args: &[&str],
) -> Vec<String> {
    let config = BTreeMap::new();
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut c = NativeContext {
        id: "staff",
        capabilities: &[],
        config: &config,
        services,
        backend: &backend_dyn,
    };
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    plugin
        .on_command(&mut c, p, name, &args)
        .expect("the staff command is handled")
        .lines
}

#[test]
fn the_commands_are_registered_under_their_nodes() {
    let (_plugin, backend, _services) = setup();
    let commands = backend.commands.lock().unwrap();
    let names: Vec<&str> = commands.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"vanish"));
    assert!(names.contains(&"spectate"));
    assert!(commands
        .iter()
        .any(|(n, node, _)| n == "vanish" && node == "aether.staff.vanish"));
    assert!(commands
        .iter()
        .any(|(n, node, _)| n == "spectate" && node == "aether.staff.spectate"));
}

#[test]
fn vanish_toggles_and_persists_the_name() {
    let (mut plugin, backend, services) = setup();
    let steve = player(7, "Steve", true);

    let lines = run(&mut plugin, &backend, &services, &steve, "vanish", &[]);
    assert!(lines[0].contains("vanished"), "{lines:?}");
    assert!(backend.staff_is_vanished("staff", 7));
    let stored = backend.kv.lock().unwrap().get("vanished").cloned().unwrap();
    assert_eq!(String::from_utf8(stored).unwrap(), "Steve");

    let lines = run(&mut plugin, &backend, &services, &steve, "vanish", &[]);
    assert!(lines[0].contains("visible"), "{lines:?}");
    assert!(!backend.staff_is_vanished("staff", 7));
    assert!(backend
        .kv
        .lock()
        .unwrap()
        .get("vanished")
        .unwrap()
        .is_empty());
}

#[test]
fn a_non_operator_is_refused() {
    let (mut plugin, backend, services) = setup();
    let guest = player(9, "Guest", false);
    let lines = run(&mut plugin, &backend, &services, &guest, "vanish", &[]);
    assert!(lines[0].contains("permission"), "{lines:?}");
    assert!(!backend.staff_is_vanished("staff", 9));
}

#[test]
fn a_persisted_vanish_is_reapplied_on_relog() {
    // Vanish Steve, then build a *fresh* plugin over the same KV (the server
    // restart) and join Steve again: the vanish must come back.
    let (mut plugin, backend, services) = setup();
    let steve = player(7, "Steve", true);
    run(&mut plugin, &backend, &services, &steve, "vanish", &[]);

    let (mut fresh, _backend, _services) = setup_over(backend.clone());
    let rejoin = player(8, "Steve", true);
    let config = BTreeMap::new();
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut c = NativeContext {
        id: "staff",
        capabilities: &[],
        config: &config,
        services: &services,
        backend: &backend_dyn,
    };
    fresh.after_player_join(&mut c, &rejoin);
    assert!(
        backend.staff_is_vanished("staff", 8),
        "the persisted vanish is re-applied on relog"
    );
}

/// Build a fresh plugin over an existing backend (a server restart).
fn setup_over(backend: Arc<Mock>) -> (Box<dyn NativePlugin>, Arc<Mock>, Arc<ServiceRegistry>) {
    let services = Arc::new(ServiceRegistry::new());
    let mut plugin = (registration().new)();
    let config = BTreeMap::new();
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut c = NativeContext {
        id: "staff",
        capabilities: &[],
        config: &config,
        services: &services,
        backend: &backend_dyn,
    };
    plugin.init(&mut c).expect("init");
    (plugin, backend, services)
}

#[test]
fn operators_see_the_vanished_on_join() {
    let (mut plugin, backend, services) = setup();
    let config = BTreeMap::new();
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut c = NativeContext {
        id: "staff",
        capabilities: &[],
        config: &config,
        services: &services,
        backend: &backend_dyn,
    };
    plugin.after_player_join(&mut c, &player(7, "Steve", true));
    assert_eq!(backend.sees.lock().unwrap().get(&7), Some(&true));
    plugin.after_player_join(&mut c, &player(9, "Guest", false));
    assert_eq!(backend.sees.lock().unwrap().get(&9), Some(&false));
}

#[test]
fn spectate_enters_on_a_target_and_auto_vanishes() {
    let (mut plugin, backend, services) = setup_with(
        vec![player(7, "Steve", true), player(8, "Alex", true)],
        BTreeMap::new(),
    );
    let steve = player(7, "Steve", true);
    let lines = run(
        &mut plugin,
        &backend,
        &services,
        &steve,
        "spectate",
        &["Alex"],
    );
    assert!(lines[0].contains("Spectating"), "{lines:?}");
    assert_eq!(*backend.enters.lock().unwrap(), vec![(7, Some(8))]);
    assert!(
        backend.staff_is_vanished("staff", 7),
        "auto-vanish happened"
    );
    assert!(backend.staff_is_spectating("staff", 7));
}

#[test]
fn spectate_off_restores_and_is_journalled() {
    let (mut plugin, backend, services) = setup();
    let steve = player(7, "Steve", true);
    run(&mut plugin, &backend, &services, &steve, "spectate", &[]);
    let lines = run(
        &mut plugin,
        &backend,
        &services,
        &steve,
        "spectate",
        &["off"],
    );
    assert!(lines[0].contains("restored"), "{lines:?}");
    assert_eq!(*backend.leaves.lock().unwrap(), vec![7]);
    assert!(backend
        .journal
        .lock()
        .unwrap()
        .iter()
        .any(|(_, a, _)| a == "spectate.off"));
}

#[test]
fn xray_is_gated_behind_spectating() {
    let (mut plugin, backend, services) = setup();
    let steve = player(7, "Steve", true);
    // Not spectating: refused, and the backend is never asked.
    let lines = run(
        &mut plugin,
        &backend,
        &services,
        &steve,
        "spectate",
        &["xray", "culling"],
    );
    assert!(lines[0].contains("only available"), "{lines:?}");
    assert!(backend.xray.lock().unwrap().is_empty());

    // Spectate, then the level goes through and is journalled.
    run(&mut plugin, &backend, &services, &steve, "spectate", &[]);
    let lines = run(
        &mut plugin,
        &backend,
        &services,
        &steve,
        "spectate",
        &["xray", "8"],
    );
    assert!(lines[0].contains("Anti-xray level set to 8"), "{lines:?}");
    assert_eq!(*backend.xray.lock().unwrap(), vec![(7, "8".to_string())]);
    assert!(backend
        .journal
        .lock()
        .unwrap()
        .iter()
        .any(|(_, a, d)| a == "xray" && d == "8"));
}

#[test]
fn the_indicator_reaches_every_vanished_player() {
    let (mut plugin, backend, services) = setup();
    let steve = player(7, "Steve", true);
    run(&mut plugin, &backend, &services, &steve, "vanish", &[]);

    let config = BTreeMap::new();
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut c = NativeContext {
        id: "staff",
        capabilities: &[],
        config: &config,
        services: &services,
        backend: &backend_dyn,
    };
    plugin.on_tick(&mut c, 20);
    let bars = backend.actionbars.lock().unwrap();
    assert!(bars
        .iter()
        .any(|(id, t)| *id == 7 && t.contains("Vanished")));
}
