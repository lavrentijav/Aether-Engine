//! The essentials-teleport plugin driven through the real host trait with a
//! recording backend. The core safe-spot search is CMD-31's; these tests check
//! the plugin's policy: permission gates, the home limit, warmup cancellation,
//! sanitisation, ownership and restart persistence.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aether_essentials_teleport_plugin::{
    registration, EssentialsTeleportPlugin, ID, NODE_DEL_HOME, NODE_DEL_WARP, NODE_HOME,
    NODE_HOMES, NODE_SET_HOME, NODE_SET_SPAWN, NODE_SET_WARP, NODE_WARP, NODE_WARPS,
};
use aether_plugin::sdk::TeleportCause;
use aether_plugin::{HostBackend, LogLevel, NativeContext, NativePlugin, Player, ServiceRegistry};

/// A recording backend: the plugin KV, the current position, permission nodes
/// and what the core was asked to teleport.
struct Mock {
    kv: Mutex<HashMap<String, Vec<u8>>>,
    position: Mutex<(String, f64, f64, f64)>,
    uuids: Mutex<HashMap<u64, u128>>,
    permissions: Mutex<HashSet<(u64, String)>>,
    messages: Mutex<Vec<(u64, String)>>,
    teleports: Mutex<Vec<(u64, f64, f64, f64, TeleportCause)>>,
    fail_teleport: AtomicBool,
}

impl Default for Mock {
    fn default() -> Self {
        Mock {
            kv: Mutex::new(HashMap::new()),
            position: Mutex::new(("overworld".to_string(), 0.0, 64.0, 0.0)),
            uuids: Mutex::new(HashMap::new()),
            permissions: Mutex::new(HashSet::new()),
            messages: Mutex::new(Vec::new()),
            teleports: Mutex::new(Vec::new()),
            fail_teleport: AtomicBool::new(false),
        }
    }
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
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect()
    }
    fn log(&self, _plugin: &str, _level: LogLevel, _message: &str) {}
    fn config(&self, _plugin: &str, _key: &str) -> Option<String> {
        None
    }
    fn supports(&self, _plugin: &str, _feature: &str) -> bool {
        true
    }
    fn player_position(&self, _plugin: &str, _player: u64) -> Option<(String, f64, f64, f64)> {
        Some(self.position.lock().unwrap().clone())
    }
    fn player_uuid(&self, _plugin: &str, player: u64) -> Option<u128> {
        self.uuids.lock().unwrap().get(&player).copied()
    }
    fn permission_check(&self, _plugin: &str, id: u64, node: &str, _op: u8) -> bool {
        self.permissions
            .lock()
            .unwrap()
            .contains(&(id, node.to_string()))
    }
    fn send_message(&self, _plugin: &str, player: u64, text: &str) {
        self.messages
            .lock()
            .unwrap()
            .push((player, text.to_string()));
    }
    fn teleport_player(
        &self,
        _plugin: &str,
        player: u64,
        x: f64,
        y: f64,
        z: f64,
        cause: TeleportCause,
    ) -> Result<(), String> {
        if self.fail_teleport.load(Ordering::SeqCst) {
            return Err("no safe spot to teleport to".to_string());
        }
        self.teleports
            .lock()
            .unwrap()
            .push((player, x, y, z, cause));
        Ok(())
    }
}

fn player(id: u64, name: &str) -> Player {
    Player {
        id,
        name: name.to_string(),
        operator: false,
    }
}

const BASE_NODES: &[&str] = &[
    NODE_SET_HOME,
    NODE_HOME,
    NODE_DEL_HOME,
    NODE_HOMES,
    NODE_SET_WARP,
    NODE_WARP,
    NODE_DEL_WARP,
    NODE_WARPS,
    NODE_SET_SPAWN,
];

struct Fixture {
    plugin: EssentialsTeleportPlugin,
    backend: Arc<Mock>,
    services: Arc<ServiceRegistry>,
    config: BTreeMap<String, String>,
}

impl Fixture {
    fn new() -> Self {
        Fixture::with_config(BTreeMap::new())
    }

    fn with_config(config: BTreeMap<String, String>) -> Self {
        let backend = Arc::new(Mock::default());
        for id in [7, 8] {
            for node in BASE_NODES {
                backend
                    .permissions
                    .lock()
                    .unwrap()
                    .insert((id, (*node).to_string()));
            }
        }
        let services = Arc::new(ServiceRegistry::new());
        let mut fixture = Fixture {
            plugin: EssentialsTeleportPlugin::default(),
            backend,
            services,
            config,
        };
        fixture.init();
        fixture
    }

    fn init(&mut self) {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        self.plugin.init(&mut ctx).expect("init");
    }

    fn restart(&mut self) {
        self.plugin = EssentialsTeleportPlugin::default();
        self.init();
    }

    fn grant(&self, id: u64, node: &str) {
        self.backend
            .permissions
            .lock()
            .unwrap()
            .insert((id, node.to_string()));
    }

    fn revoke(&self, id: u64, node: &str) {
        self.backend
            .permissions
            .lock()
            .unwrap()
            .remove(&(id, node.to_string()));
    }

    fn set_position(&self, world: &str, x: f64, y: f64, z: f64) {
        *self.backend.position.lock().unwrap() = (world.to_string(), x, y, z);
    }

    fn run(&mut self, p: &Player, name: &str, args: &[&str]) -> Vec<String> {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        self.plugin
            .on_command(&mut ctx, p, name, &args)
            .unwrap_or_else(|| panic!("/{name} is handled"))
            .lines
    }

    fn tick(&mut self, n: u64) {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        self.plugin.on_tick(&mut ctx, n);
    }

    fn damage(&mut self, entity: u64) {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        self.plugin.after_damage(&mut ctx, entity, 1.0, "test");
    }

    fn death(&mut self, entity: u64) {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        self.plugin.after_entity_death(&mut ctx, entity, None);
    }

    fn leave(&mut self, p: &Player) {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        self.plugin.after_player_leave(&mut ctx, p);
    }

    fn teleports(&self) -> Vec<(u64, f64, f64, f64, TeleportCause)> {
        self.backend.teleports.lock().unwrap().clone()
    }

    fn messages(&self) -> Vec<String> {
        self.backend
            .messages
            .lock()
            .unwrap()
            .iter()
            .map(|(_, text)| text.clone())
            .collect()
    }
}

fn text(lines: &[String]) -> String {
    lines.join(" ")
}

fn warmup(ticks: u64) -> BTreeMap<String, String> {
    BTreeMap::from([("warmup-ticks".to_string(), ticks.to_string())])
}

#[test]
fn commands_are_registered_and_spawn_is_not_overridden() {
    let manifest = registration().manifest;
    for name in [
        "sethome", "home", "delhome", "homes", "setwarp", "warp", "delwarp", "warps", "setspawn",
    ] {
        assert!(manifest.serves_command(name), "/{name} is declared");
    }
    assert!(
        !manifest.serves_command("spawn"),
        "core /spawn must never be registered by the plugin"
    );
    assert!(manifest
        .command("sethome")
        .is_some_and(|command| command.node(ID) == NODE_SET_HOME));
}

#[test]
fn sethome_needs_no_bed_or_anchor() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 12.5, 70.0, -3.5);

    let lines = fixture.run(&steve, "sethome", &[]);
    assert!(text(&lines).contains("Home `home` set"), "{lines:?}");

    let lines = fixture.run(&steve, "homes", &[]);
    assert!(text(&lines).contains("home"), "{lines:?}");
}

#[test]
fn homes_are_capped_by_the_permission_limit_then_raise_it() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 1.0, 64.0, 1.0);

    assert!(text(&fixture.run(&steve, "sethome", &[])).contains("set"));
    let lines = fixture.run(&steve, "sethome", &["second"]);
    assert!(text(&lines).contains("home limit (1)"), "{lines:?}");

    fixture.grant(7, "essentials.sethome.multiple.3");
    fixture.set_position("overworld", 2.0, 64.0, 2.0);
    let lines = fixture.run(&steve, "sethome", &["second"]);
    assert!(text(&lines).contains("set"), "{lines:?}");
    fixture.set_position("overworld", 3.0, 64.0, 3.0);
    let lines = fixture.run(&steve, "sethome", &["third"]);
    assert!(text(&lines).contains("set"), "{lines:?}");
    fixture.set_position("overworld", 4.0, 64.0, 4.0);
    let lines = fixture.run(&steve, "sethome", &["fourth"]);
    assert!(text(&lines).contains("home limit (3)"), "{lines:?}");

    let lines = fixture.run(&steve, "homes", &[]);
    assert!(text(&lines).contains("Homes (3/3)"), "{lines:?}");
}

#[test]
fn delhome_unknown_is_an_error() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let lines = fixture.run(&steve, "delhome", &["missing"]);
    assert!(text(&lines).contains("do not have a home"), "{lines:?}");
}

#[test]
fn delhome_without_a_name_asks_for_one_and_deletes_nothing() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 1.0, 64.0, 1.0);
    fixture.run(&steve, "sethome", &[]);

    let lines = fixture.run(&steve, "delhome", &[]);
    assert!(text(&lines).contains("Usage: /delhome <name>"), "{lines:?}");

    let lines = fixture.run(&steve, "homes", &[]);
    assert!(text(&lines).contains("home"), "the default home survives");
}

#[test]
fn warmup_cancels_when_the_player_moves() {
    let mut fixture = Fixture::with_config(warmup(10));
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "sethome", &[]);

    let lines = fixture.run(&steve, "home", &[]);
    assert!(text(&lines).contains("in 1 second"), "{lines:?}");

    fixture.set_position("overworld", 3.0, 64.0, 0.0);
    fixture.tick(1);
    assert!(fixture
        .messages()
        .iter()
        .any(|line| line.contains("you moved")));
    fixture.tick(10);
    assert!(fixture.teleports().is_empty());
}

#[test]
fn warmup_cancels_on_damage() {
    let mut fixture = Fixture::with_config(warmup(10));
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "sethome", &[]);

    fixture.run(&steve, "home", &[]);
    fixture.damage(7);
    fixture.tick(10);
    assert!(fixture
        .messages()
        .iter()
        .any(|line| line.contains("took damage")));
    assert!(fixture.teleports().is_empty());
}

#[test]
fn warmup_finishes_and_asks_for_a_safe_teleport() {
    let mut fixture = Fixture::with_config(warmup(10));
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 1.5, 70.0, 2.5);
    fixture.run(&steve, "sethome", &[]);
    fixture.set_position("overworld", 9.0, 64.0, 9.0);

    fixture.run(&steve, "home", &[]);
    fixture.tick(9);
    assert!(fixture.teleports().is_empty(), "not before the warmup");
    fixture.tick(10);

    let teleports = fixture.teleports();
    assert_eq!(teleports.len(), 1);
    assert_eq!(
        (teleports[0].1, teleports[0].2, teleports[0].3),
        (1.5, 70.0, 2.5)
    );
    assert_eq!(teleports[0].4, TeleportCause::Plugin);
}

#[test]
fn warp_warmup_applies_too() {
    let mut fixture = Fixture::with_config(warmup(5));
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 4.0, 65.0, 4.0);
    fixture.run(&steve, "setwarp", &["market"]);
    fixture.set_position("overworld", 0.0, 64.0, 0.0);

    fixture.run(&steve, "warp", &["market"]);
    fixture.tick(5);
    assert_eq!(fixture.teleports().len(), 1);
}

#[test]
fn a_safe_spot_failure_is_reported_not_ignored() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "sethome", &[]);
    fixture.backend.fail_teleport.store(true, Ordering::SeqCst);

    let lines = fixture.run(&steve, "home", &[]);
    assert!(text(&lines).contains("failed"), "{lines:?}");
    assert!(text(&lines).contains("no safe spot"), "{lines:?}");
}

#[test]
fn a_home_in_another_world_is_refused_by_the_intra_world_api() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "sethome", &[]);
    fixture.set_position("the_nether", 0.0, 64.0, 0.0);

    let lines = fixture.run(&steve, "home", &[]);
    assert!(text(&lines).contains("cross-world"), "{lines:?}");
    assert!(fixture.teleports().is_empty());
}

#[test]
fn warp_unknown_errors_and_setwarp_is_permission_gated() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");

    let lines = fixture.run(&steve, "warp", &["nowhere"]);
    assert!(text(&lines).contains("no warp"), "{lines:?}");

    fixture.revoke(7, NODE_SET_WARP);
    let lines = fixture.run(&steve, "setwarp", &["vip"]);
    assert!(text(&lines).contains("do not have permission"), "{lines:?}");
    let lines = fixture.run(&steve, "warps", &[]);
    assert!(text(&lines).contains("no warps"), "{lines:?}");

    fixture.grant(7, NODE_SET_WARP);
    let lines = fixture.run(&steve, "setwarp", &["vip"]);
    assert!(text(&lines).contains("set"), "{lines:?}");
}

#[test]
fn warp_names_are_sanitised_and_listed_in_order() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.run(&steve, "setwarp", &["  Zeta Place!! "]);
    fixture.run(&steve, "setwarp", &["alpha"]);

    let lines = fixture.run(&steve, "warps", &[]);
    let listing = text(&lines);
    assert!(listing.contains("alpha, zeta_place"), "{lines:?}");

    let lines = fixture.run(&steve, "warp", &["ZETA_PLACE"]);
    assert!(text(&lines).contains("Teleported"), "{lines:?}");
}

#[test]
fn homes_warps_and_spawn_survive_a_restart() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 11.0, 66.0, -7.0);
    fixture.run(&steve, "sethome", &["base"]);
    fixture.set_position("overworld", 30.0, 70.0, 30.0);
    fixture.run(&steve, "setwarp", &["market"]);
    fixture.set_position("overworld", 50.0, 80.0, 50.0);
    fixture.run(&steve, "setspawn", &[]);

    fixture.restart();

    let lines = fixture.run(&steve, "homes", &[]);
    assert!(text(&lines).contains("base"), "{lines:?}");
    let lines = fixture.run(&steve, "warps", &[]);
    assert!(text(&lines).contains("market"), "{lines:?}");
    assert_eq!(
        fixture.plugin.spawn_point(),
        Some(("overworld".to_string(), 50.0, 80.0, 50.0))
    );

    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "home", &["base"]);
    let teleports = fixture.teleports();
    assert_eq!(
        (teleports[0].1, teleports[0].2, teleports[0].3),
        (11.0, 66.0, -7.0)
    );
}

#[test]
fn one_player_cannot_replace_another_players_home() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");

    fixture.set_position("overworld", 1.0, 64.0, 1.0);
    fixture.run(&steve, "sethome", &["base"]);
    fixture.set_position("overworld", 9.0, 64.0, 9.0);
    fixture.run(&alex, "sethome", &["base"]);

    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "home", &["base"]);
    fixture.run(&alex, "home", &["base"]);

    let teleports = fixture.teleports();
    assert_eq!(
        (teleports[0].1, teleports[0].2, teleports[0].3),
        (1.0, 64.0, 1.0),
        "Steve keeps his own `base`"
    );
    assert_eq!(
        (teleports[1].1, teleports[1].2, teleports[1].3),
        (9.0, 64.0, 9.0),
        "Alex has a separate `base`"
    );

    fixture.run(&alex, "delhome", &["base"]);
    let lines = fixture.run(&steve, "homes", &[]);
    assert!(text(&lines).contains("base"), "{lines:?}");
}

#[test]
fn a_uuid_keeps_homes_after_a_rename() {
    let mut fixture = Fixture::new();
    fixture
        .backend
        .uuids
        .lock()
        .unwrap()
        .insert(7, 0x1234_5678_9abc_def0);
    fixture.set_position("overworld", 5.0, 64.0, 5.0);
    fixture.run(&player(7, "Steve"), "sethome", &["base"]);

    fixture.restart();
    let renamed = player(7, "Steven");
    let lines = fixture.run(&renamed, "homes", &[]);
    assert!(text(&lines).contains("base"), "{lines:?}");
}

#[test]
fn warmup_cancels_on_death_and_leave() {
    let mut fixture = Fixture::with_config(warmup(10));
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "sethome", &[]);
    fixture.run(&alex, "sethome", &[]);

    fixture.run(&steve, "home", &[]);
    fixture.death(7);
    fixture.tick(10);
    assert!(fixture
        .messages()
        .iter()
        .any(|line| line.contains("you died")));

    fixture.run(&alex, "home", &[]);
    fixture.leave(&alex);
    fixture.tick(30);
    assert!(
        fixture.teleports().is_empty(),
        "dead and disconnected players are not moved"
    );
}

#[test]
fn setspawn_is_permission_gated() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.revoke(7, NODE_SET_SPAWN);

    let lines = fixture.run(&steve, "setspawn", &[]);
    assert!(text(&lines).contains("do not have permission"), "{lines:?}");
    assert_eq!(fixture.plugin.spawn_point(), None);

    fixture.grant(7, NODE_SET_SPAWN);
    fixture.set_position("overworld", 7.0, 71.0, 7.0);
    let lines = fixture.run(&steve, "setspawn", &[]);
    assert!(text(&lines).contains("Plugin spawn set"), "{lines:?}");
    assert_eq!(
        fixture.plugin.spawn_point(),
        Some(("overworld".to_string(), 7.0, 71.0, 7.0))
    );
}

#[test]
fn delwarp_is_permission_gated_and_unknown_is_an_error() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 1.0, 64.0, 1.0);
    fixture.run(&steve, "setwarp", &["market"]);

    let lines = fixture.run(&steve, "delwarp", &["nowhere"]);
    assert!(text(&lines).contains("no warp"), "{lines:?}");

    fixture.revoke(7, NODE_DEL_WARP);
    let lines = fixture.run(&steve, "delwarp", &["market"]);
    assert!(text(&lines).contains("do not have permission"), "{lines:?}");
    let lines = fixture.run(&steve, "warps", &[]);
    assert!(text(&lines).contains("market"), "the warp survives");

    fixture.grant(7, NODE_DEL_WARP);
    let lines = fixture.run(&steve, "delwarp", &["market"]);
    assert!(text(&lines).contains("deleted"), "{lines:?}");
    let lines = fixture.run(&steve, "warps", &[]);
    assert!(text(&lines).contains("no warps"), "{lines:?}");
}

#[test]
fn replacing_an_existing_home_is_allowed_at_the_limit() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_position("overworld", 1.0, 64.0, 1.0);
    fixture.run(&steve, "sethome", &[]);
    // The cap is 1, but moving the home the player already owns is not a new
    // name and must not be refused.
    fixture.set_position("overworld", 8.0, 65.0, 8.0);
    let lines = fixture.run(&steve, "sethome", &[]);
    assert!(text(&lines).contains("set"), "{lines:?}");

    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "home", &[]);
    let teleports = fixture.teleports();
    assert_eq!(
        (teleports[0].1, teleports[0].2, teleports[0].3),
        (8.0, 65.0, 8.0),
        "the replacement location is what `/home` uses"
    );
}

#[test]
fn interleaved_players_keep_separate_warmups_and_homes() {
    let mut fixture = Fixture::with_config(warmup(5));
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");

    fixture.set_position("overworld", 1.0, 64.0, 1.0);
    fixture.run(&steve, "sethome", &["base"]);
    fixture.set_position("overworld", 2.0, 64.0, 2.0);
    fixture.run(&alex, "sethome", &["base"]);

    // Both start a warmup; then they interleave unrelated commands and ticks.
    fixture.set_position("overworld", 0.0, 64.0, 0.0);
    fixture.run(&steve, "home", &["base"]);
    fixture.run(&alex, "home", &["base"]);
    fixture.run(&alex, "setwarp", &["shared"]);
    fixture.tick(5);

    let teleports = fixture.teleports();
    assert_eq!(teleports.len(), 2, "both pending moves land");
    let find = |player: u64| {
        teleports
            .iter()
            .find(|t| t.0 == player)
            .map(|t| (t.1, t.2, t.3))
            .expect("that player's move landed")
    };
    assert_eq!(find(7), (1.0, 64.0, 1.0));
    assert_eq!(find(8), (2.0, 64.0, 2.0));

    // Deleting one player's home does not remove the other's same-named one.
    fixture.run(&steve, "delhome", &["base"]);
    let lines = fixture.run(&alex, "homes", &[]);
    assert!(text(&lines).contains("base"), "{lines:?}");
}
