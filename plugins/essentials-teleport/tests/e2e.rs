//! The essentials-teleport plugin driven through the real host trait with a
//! recording backend. The core safe-spot search is CMD-31's; these tests check
//! the plugin's policy: permission gates, the home limit, warmup cancellation,
//! sanitisation, ownership and restart persistence.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aether_essentials_teleport_plugin::{
    registration, EssentialsTeleportPlugin, ID, NODE_BACK, NODE_DEL_HOME, NODE_DEL_WARP, NODE_HOME,
    NODE_HOMES, NODE_JUMP, NODE_RTP, NODE_SET_HOME, NODE_SET_SPAWN, NODE_SET_WARP, NODE_TOP,
    NODE_TPA, NODE_TPA_HERE, NODE_TP_ACCEPT, NODE_TP_ALL, NODE_TP_CANCEL, NODE_TP_DENY,
    NODE_TP_HERE, NODE_TP_POS, NODE_UP, NODE_WARP, NODE_WARPS,
};
use aether_plugin::sdk::TeleportCause;
use aether_plugin::{
    HostBackend, LogLevel, NativeContext, NativePlugin, Player, ServiceRegistry, WorldBlock,
};

#[derive(Clone, Debug)]
struct TestBlock {
    name: String,
    solid: bool,
    hazard: bool,
}

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
    positions: Mutex<HashMap<u64, (String, f64, f64, f64)>>,
    online: Mutex<BTreeMap<u64, Player>>,
    looks: Mutex<HashMap<u64, (f32, f32)>>,
    blocks: Mutex<HashMap<(i32, i32, i32), TestBlock>>,
    height: Mutex<(i32, i32)>,
    ignores: Mutex<HashSet<(u64, u64)>>,
    safe_spot_calls: Mutex<Vec<((i32, i32, i32), u32, bool)>>,
    random_calls: Mutex<Vec<((i32, i32, i32), u32, u32)>>,
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
            positions: Mutex::new(HashMap::new()),
            online: Mutex::new(BTreeMap::new()),
            looks: Mutex::new(HashMap::new()),
            blocks: Mutex::new(HashMap::new()),
            height: Mutex::new((-64, 383)),
            ignores: Mutex::new(HashSet::new()),
            safe_spot_calls: Mutex::new(Vec::new()),
            random_calls: Mutex::new(Vec::new()),
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
    fn player_position(&self, _plugin: &str, player: u64) -> Option<(String, f64, f64, f64)> {
        self.positions
            .lock()
            .unwrap()
            .get(&player)
            .cloned()
            .or_else(|| Some(self.position.lock().unwrap().clone()))
    }
    fn player_look(&self, _plugin: &str, player: u64) -> Option<(f32, f32)> {
        if !self.online.lock().unwrap().contains_key(&player) {
            return None;
        }
        Some(
            self.looks
                .lock()
                .unwrap()
                .get(&player)
                .copied()
                .unwrap_or((0.0, 0.0)),
        )
    }
    fn world_block(&self, _plugin: &str, x: i32, y: i32, z: i32) -> Option<WorldBlock> {
        let block = self.blocks.lock().unwrap().get(&(x, y, z)).cloned();
        Some(match block {
            Some(block) => WorldBlock {
                name: block.name,
                solid: block.solid,
                hazard: block.hazard,
            },
            None => WorldBlock {
                name: "minecraft:air".to_string(),
                solid: false,
                hazard: false,
            },
        })
    }
    fn world_height(&self, _plugin: &str) -> Option<(i32, i32)> {
        Some(*self.height.lock().unwrap())
    }
    fn find_safe_spot(
        &self,
        _plugin: &str,
        around: (i32, i32, i32),
        radius: u32,
        prefer_surface: bool,
    ) -> Option<(i32, i32, i32)> {
        self.safe_spot_calls
            .lock()
            .unwrap()
            .push((around, radius, prefer_surface));
        self.nearest_safe(around, radius)
    }
    fn random_safe_spot(
        &self,
        _plugin: &str,
        center: (i32, i32, i32),
        min_radius: u32,
        max_radius: u32,
    ) -> Option<(i32, i32, i32)> {
        self.random_calls
            .lock()
            .unwrap()
            .push((center, min_radius, max_radius));
        // Deterministic stand-in for the core search: the nearest column's
        // safe spot, bounded by the requested radius.
        self.nearest_safe(center, max_radius.min(8))
    }
    fn player_ignores(&self, _plugin: &str, player: u64, other: u64) -> bool {
        self.ignores.lock().unwrap().contains(&(player, other))
    }
    fn player_uuid(&self, _plugin: &str, player: u64) -> Option<u128> {
        self.uuids.lock().unwrap().get(&player).copied()
    }
    fn online_players(&self, _plugin: &str) -> Vec<Player> {
        self.online.lock().unwrap().values().cloned().collect()
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

impl Mock {
    fn block_at(&self, x: i32, y: i32, z: i32) -> Option<TestBlock> {
        self.blocks.lock().unwrap().get(&(x, y, z)).cloned()
    }

    fn solid_at(&self, x: i32, y: i32, z: i32) -> bool {
        self.block_at(x, y, z).is_some_and(|block| block.solid)
    }

    fn hazard_at(&self, x: i32, y: i32, z: i32) -> bool {
        self.block_at(x, y, z).is_some_and(|block| block.hazard)
    }

    fn safe_at(&self, x: i32, y: i32, z: i32, min_y: i32, max_y: i32) -> bool {
        if y <= min_y || y >= max_y {
            return false;
        }
        if !self.solid_at(x, y - 1, z) {
            return false;
        }
        if self.solid_at(x, y, z) || self.solid_at(x, y + 1, z) {
            return false;
        }
        (-1..=1).all(|dy| !self.hazard_at(x, y + dy, z))
    }

    fn nearest_safe(&self, around: (i32, i32, i32), radius: u32) -> Option<(i32, i32, i32)> {
        let (min_y, max_y) = *self.height.lock().unwrap();
        let r = radius.min(8) as i32;
        for ring in 0..=r {
            for dx in -ring..=ring {
                for dz in -ring..=ring {
                    if ring > 0 && dx.abs() != ring && dz.abs() != ring {
                        continue;
                    }
                    let x = around.0 + dx;
                    let z = around.2 + dz;
                    for y in (min_y + 1..max_y).rev() {
                        if self.safe_at(x, y, z, min_y, max_y) {
                            return Some((x, y, z));
                        }
                    }
                }
            }
        }
        None
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
    NODE_TPA,
    NODE_TPA_HERE,
    NODE_TP_ACCEPT,
    NODE_TP_DENY,
    NODE_TP_CANCEL,
    NODE_TP_ALL,
    NODE_TP_HERE,
    NODE_TP_POS,
    NODE_BACK,
    NODE_TOP,
    NODE_JUMP,
    NODE_UP,
    NODE_RTP,
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
        for p in [player(7, "Steve"), player(8, "Alex")] {
            backend.online.lock().unwrap().insert(p.id, p);
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

    fn set_player_position(&self, id: u64, world: &str, x: f64, y: f64, z: f64) {
        self.backend
            .positions
            .lock()
            .unwrap()
            .insert(id, (world.to_string(), x, y, z));
    }

    fn set_look(&self, id: u64, yaw: f32, pitch: f32) {
        self.backend.looks.lock().unwrap().insert(id, (yaw, pitch));
    }

    fn add_player(&self, p: &Player) {
        self.backend.online.lock().unwrap().insert(p.id, p.clone());
        for node in BASE_NODES {
            self.grant(p.id, node);
        }
    }

    fn remove_player(&self, id: u64) {
        self.backend.online.lock().unwrap().remove(&id);
        self.backend.positions.lock().unwrap().remove(&id);
    }

    fn ignore(&self, player: u64, other: u64) {
        self.backend.ignores.lock().unwrap().insert((player, other));
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str, solid: bool, hazard: bool) {
        self.backend.blocks.lock().unwrap().insert(
            (x, y, z),
            TestBlock {
                name: name.to_string(),
                solid,
                hazard,
            },
        );
    }

    fn set_height(&self, min_y: i32, max_y: i32) {
        *self.backend.height.lock().unwrap() = (min_y, max_y);
    }

    fn messages_to(&self, id: u64) -> Vec<String> {
        self.backend
            .messages
            .lock()
            .unwrap()
            .iter()
            .filter(|(player, _)| *player == id)
            .map(|(_, text)| text.clone())
            .collect()
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
        "tpa", "tpahere", "tpaccept", "tpdeny", "tpcancel", "tpall", "tphere", "tppos", "back",
        "top", "jump", "up", "rtp",
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

fn config(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn tpa_refuses_self_offline_and_ignored_targets() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 5.0, 64.0, 5.0);

    let lines = fixture.run(&steve, "tpa", &["steve"]);
    assert!(text(&lines).contains("yourself"), "{lines:?}");
    let lines = fixture.run(&steve, "tpa", &["Nobody"]);
    assert!(text(&lines).contains("not online"), "{lines:?}");

    fixture.ignore(8, 7);
    let lines = fixture.run(&steve, "tpa", &["alex"]);
    assert!(text(&lines).contains("ignoring"), "{lines:?}");
    assert!(fixture.teleports().is_empty());
}

#[test]
fn tpaccept_without_a_request_errors() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let lines = fixture.run(&steve, "tpaccept", &[]);
    assert!(text(&lines).contains("no pending"), "{lines:?}");
}

#[test]
fn tpa_moves_the_requester_to_the_target_on_accept() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 10.0, 70.0, -4.0);

    let lines = fixture.run(&steve, "tpa", &["alex"]);
    assert!(text(&lines).contains("sent"), "{lines:?}");
    assert!(fixture
        .messages_to(8)
        .iter()
        .any(|line| line.contains("requested to teleport to you")));

    let lines = fixture.run(&alex, "tpaccept", &[]);
    assert!(text(&lines).contains("Accepted"), "{lines:?}");
    let teleports = fixture.teleports();
    assert_eq!(teleports.len(), 1);
    assert_eq!(teleports[0].0, 7, "the requester moves");
    assert_eq!(
        (teleports[0].1, teleports[0].2, teleports[0].3),
        (10.0, 70.0, -4.0)
    );
}

#[test]
fn tpahere_moves_the_target_to_the_requester() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_player_position(7, "overworld", 3.0, 65.0, 3.0);
    fixture.set_player_position(8, "overworld", 8.0, 64.0, 8.0);

    fixture.run(&steve, "tpahere", &["alex"]);
    assert!(fixture
        .messages_to(8)
        .iter()
        .any(|line| line.contains("teleport to them")));
    let lines = fixture.run(&alex, "tpaccept", &[]);
    assert!(text(&lines).contains("Accepted"), "{lines:?}");

    let teleports = fixture.teleports();
    assert_eq!(teleports.len(), 1);
    assert_eq!(teleports[0].0, 8, "the target moves");
    assert_eq!(
        (teleports[0].1, teleports[0].2, teleports[0].3),
        (3.0, 65.0, 3.0)
    );
}

#[test]
fn multiple_requests_can_be_accepted_by_name_and_denied_together() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    let casey = player(9, "Casey");
    fixture.add_player(&casey);
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);
    fixture.set_player_position(9, "overworld", 9.0, 64.0, 9.0);

    fixture.run(&steve, "tpa", &["casey"]);
    fixture.run(&alex, "tpa", &["casey"]);
    let lines = fixture.run(&casey, "tpaccept", &["alex"]);
    assert!(text(&lines).contains("Accepted"), "{lines:?}");
    assert_eq!(fixture.teleports().len(), 1);
    assert_eq!(fixture.teleports()[0].0, 8);

    let lines = fixture.run(&casey, "tpdeny", &[]);
    assert!(text(&lines).contains("Denied 1"), "{lines:?}");
    assert!(fixture
        .messages_to(7)
        .iter()
        .any(|line| line.contains("denied your teleport request")));

    let lines = fixture.run(&casey, "tpaccept", &["steve"]);
    assert!(text(&lines).contains("no pending"), "{lines:?}");
}

#[test]
fn requests_expire_and_notify_both_sides() {
    let mut fixture = Fixture::with_config(config(&[("request-timeout-ticks", "5")]));
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);

    fixture.run(&steve, "tpa", &["alex"]);
    fixture.tick(5);
    assert!(fixture
        .messages_to(7)
        .iter()
        .any(|line| line.contains("expired")));
    assert!(fixture
        .messages_to(8)
        .iter()
        .any(|line| line.contains("expired")));

    let lines = fixture.run(&alex, "tpaccept", &[]);
    assert!(text(&lines).contains("no pending"), "{lines:?}");
}

#[test]
fn accepting_a_request_from_an_offline_requester_errors() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);

    fixture.run(&steve, "tpa", &["alex"]);
    fixture.remove_player(7);
    let lines = fixture.run(&alex, "tpaccept", &[]);
    assert!(text(&lines).contains("no longer online"), "{lines:?}");
    assert!(fixture.teleports().is_empty());
}

#[test]
fn tpcancel_drops_outgoing_requests() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);

    fixture.run(&steve, "tpa", &["alex"]);
    let lines = fixture.run(&steve, "tpcancel", &[]);
    assert!(text(&lines).contains("Cancelled 1"), "{lines:?}");
    assert!(fixture
        .messages_to(8)
        .iter()
        .any(|line| line.contains("cancelled their teleport request")));
    let lines = fixture.run(&alex, "tpaccept", &[]);
    assert!(text(&lines).contains("no pending"), "{lines:?}");
}

#[test]
fn back_after_death_returns_to_the_death_spot() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_player_position(7, "overworld", 1.5, 64.0, 2.5);
    fixture.death(7);
    fixture.set_player_position(7, "overworld", 20.0, 64.0, 20.0);

    let lines = fixture.run(&steve, "back", &[]);
    assert!(text(&lines).contains("Teleported"), "{lines:?}");
    let teleports = fixture.teleports();
    assert_eq!(teleports.len(), 1);
    assert_eq!(
        (teleports[0].1, teleports[0].2, teleports[0].3),
        (1.5, 64.0, 2.5)
    );
}

#[test]
fn back_after_a_teleport_returns_to_the_pre_teleport_spot() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.run(&steve, "sethome", &["base"]);
    fixture.set_player_position(7, "overworld", 5.0, 66.0, 5.0);

    fixture.run(&steve, "home", &["base"]);
    assert_eq!(fixture.teleports()[0].1, 1.0);
    let lines = fixture.run(&steve, "back", &[]);
    assert!(text(&lines).contains("Teleported"), "{lines:?}");
    let last = fixture.teleports().last().cloned().expect("the back move");
    assert_eq!((last.1, last.2, last.3), (5.0, 66.0, 5.0));
}

#[test]
fn back_after_a_warp_returns_to_the_pre_warp_spot() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_player_position(7, "overworld", 4.0, 65.0, 4.0);
    fixture.run(&steve, "setwarp", &["market"]);
    fixture.set_player_position(7, "overworld", 12.0, 64.0, 12.0);

    fixture.run(&steve, "warp", &["market"]);
    fixture.run(&steve, "back", &[]);
    let last = fixture.teleports().last().cloned().expect("the back move");
    assert_eq!((last.1, last.2, last.3), (12.0, 64.0, 12.0));
}

#[test]
fn back_is_set_by_an_accepted_tpa() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_player_position(7, "overworld", 2.0, 64.0, 2.0);
    fixture.set_player_position(8, "overworld", 9.0, 64.0, 9.0);

    fixture.run(&steve, "tpa", &["alex"]);
    fixture.run(&alex, "tpaccept", &[]);
    fixture.run(&steve, "back", &[]);
    let last = fixture.teleports().last().cloned().expect("the back move");
    assert_eq!((last.1, last.2, last.3), (2.0, 64.0, 2.0));
}

#[test]
fn back_survives_a_restart() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_player_position(7, "overworld", 3.0, 65.0, 3.0);
    fixture.death(7);
    fixture.restart();
    fixture.set_player_position(7, "overworld", 9.0, 64.0, 9.0);

    let lines = fixture.run(&steve, "back", &[]);
    assert!(text(&lines).contains("Teleported"), "{lines:?}");
    let last = fixture.teleports().last().cloned().expect("the back move");
    assert_eq!((last.1, last.2, last.3), (3.0, 65.0, 3.0));
}

#[test]
fn back_without_history_errors() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let lines = fixture.run(&steve, "back", &[]);
    assert!(text(&lines).contains("no previous location"), "{lines:?}");
    assert!(fixture.teleports().is_empty());
}

#[test]
fn top_finds_the_highest_safe_surface_and_avoids_lava() {
    let mut fixture = Fixture::with_config(config(&[("warmup-ticks", "0")]));
    fixture.set_height(0, 20);
    for y in 0..=5 {
        fixture.set_block(0, y, 0, "minecraft:stone", true, false);
    }
    fixture.set_player_position(7, "overworld", 0.5, 8.0, 0.5);

    let lines = fixture.run(&player(7, "Steve"), "top", &[]);
    assert!(text(&lines).contains("Teleported"), "{lines:?}");
    assert_eq!(fixture.teleports()[0].2, 6.0);
    let call = fixture.backend.safe_spot_calls.lock().unwrap()[0];
    assert_eq!(call.1, 0, "/top searches one column");
    assert!(!call.2, "/top must not jump to an unrelated surface hint");

    // Lava as the only candidate: no safe spot, no teleport.
    let mut lava = Fixture::new();
    lava.set_height(0, 20);
    lava.set_block(0, 5, 0, "minecraft:stone", true, false);
    lava.set_block(0, 6, 0, "minecraft:lava", false, true);
    lava.set_player_position(7, "overworld", 0.5, 8.0, 0.5);
    let lines = lava.run(&player(7, "Steve"), "top", &[]);
    assert!(text(&lines).contains("No safe spot"), "{lines:?}");
    assert!(lava.teleports().is_empty());
}

#[test]
fn top_stands_on_leaves_and_not_inside_them() {
    let mut fixture = Fixture::new();
    fixture.set_height(0, 20);
    fixture.set_block(0, 3, 0, "minecraft:stone", true, false);
    fixture.set_block(0, 4, 0, "minecraft:oak_leaves", true, false);
    fixture.set_block(0, 5, 0, "minecraft:oak_leaves", true, false);
    fixture.set_player_position(7, "overworld", 0.5, 1.0, 0.5);

    fixture.run(&player(7, "Steve"), "top", &[]);
    assert_eq!(fixture.teleports()[0].2, 6.0, "feet above the canopy");
}

#[test]
fn top_on_a_solid_bedrock_ceiling_errors() {
    let mut fixture = Fixture::new();
    fixture.set_height(0, 10);
    for y in 0..=9 {
        fixture.set_block(0, y, 0, "minecraft:bedrock", true, false);
    }
    fixture.set_player_position(7, "overworld", 0.5, 2.0, 0.5);
    let lines = fixture.run(&player(7, "Steve"), "top", &[]);
    assert!(text(&lines).contains("No safe spot"), "{lines:?}");
    assert!(fixture.teleports().is_empty());
}

#[test]
fn jump_no_ops_without_a_block_ahead_and_lands_on_the_block() {
    let mut fixture = Fixture::new();
    fixture.set_height(0, 20);
    fixture.set_player_position(7, "overworld", 0.5, 1.0, 0.5);
    fixture.set_look(7, 0.0, 0.0);

    let lines = fixture.run(&player(7, "Steve"), "jump", &[]);
    assert!(text(&lines).contains("no block ahead"), "{lines:?}");
    assert!(fixture.teleports().is_empty());

    fixture.set_block(0, 2, 3, "minecraft:stone", true, false);
    let lines = fixture.run(&player(7, "Steve"), "jump", &[]);
    assert!(text(&lines).contains("Teleported"), "{lines:?}");
    let teleports = fixture.teleports();
    assert_eq!(
        (teleports[0].1, teleports[0].2, teleports[0].3),
        (0.0, 3.0, 3.0)
    );
}

#[test]
fn up_stops_at_the_first_blocked_cell_and_never_enters_a_solid() {
    let mut fixture = Fixture::new();
    fixture.set_height(-4, 20);
    fixture.set_player_position(7, "overworld", 0.5, 1.0, 0.5);
    fixture.set_block(0, 5, 0, "minecraft:stone", true, false);

    let lines = fixture.run(&player(7, "Steve"), "up", &["10"]);
    assert!(text(&lines).contains("Teleported"), "{lines:?}");
    let teleports = fixture.teleports();
    assert_eq!(teleports[0].2, 3.0, "stops below the block at y=5");
    assert!(!fixture.backend.solid_at(0, 3, 0));
    assert!(!fixture.backend.solid_at(0, 4, 0));

    let mut blocked = Fixture::new();
    blocked.set_height(-4, 20);
    blocked.set_player_position(7, "overworld", 0.5, 1.0, 0.5);
    blocked.set_block(0, 2, 0, "minecraft:stone", true, false);
    let lines = blocked.run(&player(7, "Steve"), "up", &[]);
    assert!(text(&lines).contains("no room"), "{lines:?}");
    assert!(blocked.teleports().is_empty());
}

#[test]
fn up_is_bounded_by_the_configured_limit() {
    let mut fixture = Fixture::with_config(config(&[("max-up", "3")]));
    fixture.set_player_position(7, "overworld", 0.5, 1.0, 0.5);
    let lines = fixture.run(&player(7, "Steve"), "up", &["4"]);
    assert!(text(&lines).contains("at most 3"), "{lines:?}");
    assert!(fixture.teleports().is_empty());
}

#[test]
fn tppos_sends_absolute_and_relative_coordinates_to_the_safe_service() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    fixture.set_player_position(7, "overworld", 2.0, 64.0, 3.0);

    fixture.run(&steve, "tppos", &["4", "70", "5"]);
    assert_eq!(
        (
            fixture.teleports()[0].1,
            fixture.teleports()[0].2,
            fixture.teleports()[0].3
        ),
        (4.0, 70.0, 5.0)
    );
    fixture.run(&steve, "tppos", &["~1", "~", "~-2"]);
    let last = fixture.teleports().last().cloned().expect("relative move");
    assert_eq!((last.1, last.2, last.3), (3.0, 64.0, 1.0));
}

#[test]
fn rtp_uses_the_core_random_search_and_errors_without_a_spot() {
    let mut fixture = Fixture::new();
    fixture.set_height(0, 20);
    for x in -5..=5 {
        for z in -5..=5 {
            fixture.set_block(x, 0, z, "minecraft:grass_block", true, false);
        }
    }
    fixture.set_player_position(7, "overworld", 0.5, 8.0, 0.5);

    let lines = fixture.run(&player(7, "Steve"), "rtp", &["5"]);
    assert!(text(&lines).contains("Teleported"), "{lines:?}");
    assert_eq!(fixture.teleports().len(), 1);
    let call = fixture.backend.random_calls.lock().unwrap()[0];
    assert_eq!((call.1, call.2), (1, 5));

    let mut empty = Fixture::new();
    empty.set_height(0, 20);
    empty.set_player_position(7, "overworld", 0.5, 8.0, 0.5);
    let lines = empty.run(&player(7, "Steve"), "rtp", &[]);
    assert!(text(&lines).contains("No safe spot"), "{lines:?}");
    assert!(empty.teleports().is_empty());
}

#[test]
fn rtp_rejects_a_radius_above_the_configured_maximum() {
    let mut fixture = Fixture::with_config(config(&[("rtp-radius", "10")]));
    let lines = fixture.run(&player(7, "Steve"), "rtp", &["50"]);
    assert!(text(&lines).contains("between 1 and 10"), "{lines:?}");
}

#[test]
fn tpall_and_tphere_are_permission_gated_and_move_the_right_players() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let casey = player(9, "Casey");
    fixture.add_player(&casey);
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);
    fixture.set_player_position(9, "overworld", 3.0, 64.0, 3.0);

    let lines = fixture.run(&steve, "tpall", &[]);
    assert!(text(&lines).contains("Teleported 2"), "{lines:?}");
    let teleports = fixture.teleports();
    assert_eq!(teleports.len(), 2);
    assert!(teleports.iter().all(|t| t.0 != 7));
    assert!(teleports
        .iter()
        .all(|t| (t.1, t.2, t.3) == (1.0, 64.0, 1.0)));

    fixture.run(&steve, "tphere", &["alex"]);
    let last = fixture.teleports().last().cloned().expect("tphere");
    assert_eq!(last.0, 8);
    assert_eq!((last.1, last.2, last.3), (1.0, 64.0, 1.0));

    fixture.revoke(7, NODE_TP_ALL);
    let lines = fixture.run(&steve, "tpall", &[]);
    assert!(text(&lines).contains("do not have permission"), "{lines:?}");
    fixture.revoke(7, NODE_TP_HERE);
    let lines = fixture.run(&steve, "tphere", &["alex"]);
    assert!(text(&lines).contains("do not have permission"), "{lines:?}");
}

#[test]
fn every_new_command_checks_its_essentials_node() {
    let cases = [
        ("tpa", vec!["alex"], NODE_TPA),
        ("tpahere", vec!["alex"], NODE_TPA_HERE),
        ("tpaccept", vec![], NODE_TP_ACCEPT),
        ("tpdeny", vec![], NODE_TP_DENY),
        ("tpcancel", vec![], NODE_TP_CANCEL),
        ("tpall", vec![], NODE_TP_ALL),
        ("tphere", vec!["alex"], NODE_TP_HERE),
        ("tppos", vec!["1", "2", "3"], NODE_TP_POS),
        ("back", vec![], NODE_BACK),
        ("top", vec![], NODE_TOP),
        ("jump", vec![], NODE_JUMP),
        ("up", vec![], NODE_UP),
        ("rtp", vec![], NODE_RTP),
    ];
    for (command, args, node) in cases {
        let mut fixture = Fixture::new();
        fixture.revoke(7, node);
        let lines = fixture.run(&player(7, "Steve"), command, &args);
        assert!(
            text(&lines).contains(&format!("permission ({node})")),
            "/{command} should check {node}: {lines:?}"
        );
    }
}

#[test]
fn cooldowns_apply_and_the_bypass_node_lifts_them() {
    let mut fixture = Fixture::with_config(config(&[("cooldown-ticks", "10")]));
    let steve = player(7, "Steve");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);

    assert!(text(&fixture.run(&steve, "tpa", &["alex"])).contains("sent"));
    let lines = fixture.run(&steve, "tpa", &["alex"]);
    assert!(text(&lines).contains("Wait"), "{lines:?}");

    fixture.grant(7, "essentials.tpa.bypass");
    let lines = fixture.run(&steve, "tpa", &["alex"]);
    assert!(text(&lines).contains("updated"), "{lines:?}");
}

#[test]
fn an_accepted_tpa_warmup_cancels_when_the_requester_moves() {
    let mut fixture = Fixture::with_config(config(&[("warmup-ticks", "10")]));
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);

    fixture.run(&steve, "tpa", &["alex"]);
    fixture.run(&alex, "tpaccept", &[]);
    fixture.set_player_position(7, "overworld", 4.0, 64.0, 4.0);
    fixture.tick(1);
    fixture.tick(10);
    assert!(fixture.teleports().is_empty());
    assert!(fixture
        .messages_to(7)
        .iter()
        .any(|line| line.contains("you moved")));
}

#[test]
fn overlapping_requests_to_one_target_all_land() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    let casey = player(9, "Casey");
    fixture.add_player(&casey);
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);
    fixture.set_player_position(9, "overworld", 9.0, 64.0, 9.0);

    fixture.run(&steve, "tpa", &["casey"]);
    fixture.run(&alex, "tpa", &["casey"]);
    fixture.run(&casey, "tpaccept", &["steve"]);
    fixture.run(&casey, "tpaccept", &["alex"]);

    let teleports = fixture.teleports();
    assert_eq!(teleports.len(), 2);
    assert!(teleports.iter().any(|t| t.0 == 7));
    assert!(teleports.iter().any(|t| t.0 == 8));
}

#[test]
fn requests_do_not_survive_a_restart() {
    let mut fixture = Fixture::new();
    let steve = player(7, "Steve");
    let alex = player(8, "Alex");
    fixture.set_player_position(7, "overworld", 1.0, 64.0, 1.0);
    fixture.set_player_position(8, "overworld", 2.0, 64.0, 2.0);
    fixture.run(&steve, "tpa", &["alex"]);
    fixture.restart();
    let lines = fixture.run(&alex, "tpaccept", &[]);
    assert!(text(&lines).contains("no pending"), "{lines:?}");
}
