//! The claims policy driven through the real host trait with a recording
//! backend. The fast index and the protection verdict are the core's (CMD-29)
//! and are covered by `aether-server`; these tests check the plugin's policy:
//! limits, overlap refusal, ownership, persistence and the shovel state.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use aether_plugin::{
    Decision, HostBackend, LogLevel, NativeContext, NativePlugin, Player, RegionInfo,
    ServiceRegistry,
};

use aether_claims_plugin::{
    registration, NODE_ABANDON, NODE_ABANDON_ALL, NODE_ADMIN, NODE_CLAIM, NODE_LIST,
    NODE_LIST_OTHERS, PLUGIN_ID,
};

const OVERWORLD: &str = "minecraft:overworld";

#[derive(Default)]
struct Mock {
    players: Mutex<HashMap<u64, (String, String, f64, f64, f64)>>,
    permissions: Mutex<HashSet<(u64, String)>>,
    kv: Mutex<HashMap<String, Vec<u8>>>,
    regions: Mutex<BTreeMap<u64, RegionInfo>>,
    messages: Mutex<Vec<(u64, String)>>,
    config: Mutex<BTreeMap<String, String>>,
}

impl Mock {
    fn add_player(&self, id: u64, name: &str, x: f64, z: f64) {
        self.players
            .lock()
            .unwrap()
            .insert(id, (name.to_string(), OVERWORLD.to_string(), x, 64.0, z));
    }

    fn grant_all(&self, id: u64) {
        let mut perms = self.permissions.lock().unwrap();
        for node in [
            NODE_CLAIM,
            NODE_ABANDON,
            NODE_ABANDON_ALL,
            NODE_LIST,
            NODE_LIST_OTHERS,
            NODE_ADMIN,
        ] {
            perms.insert((id, node.to_string()));
        }
    }

    fn clear_messages(&self) {
        self.messages.lock().unwrap().clear();
    }

    fn messages(&self) -> Vec<String> {
        self.messages
            .lock()
            .unwrap()
            .iter()
            .map(|(_, text)| text.clone())
            .collect()
    }

    fn last_message(&self) -> String {
        self.messages
            .lock()
            .unwrap()
            .last()
            .map(|(_, text)| text.clone())
            .unwrap_or_default()
    }

    fn claim_regions(&self) -> Vec<RegionInfo> {
        self.regions
            .lock()
            .unwrap()
            .values()
            .filter(|region| region.source.as_deref() == Some(PLUGIN_ID))
            .cloned()
            .collect()
    }

    fn add_region(&self, region: RegionInfo) {
        self.regions.lock().unwrap().insert(region.id, region);
    }
}

fn same_world(a: &str, b: &str) -> bool {
    a.trim_start_matches("minecraft:")
        .eq_ignore_ascii_case(b.trim_start_matches("minecraft:"))
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

    fn config(&self, _plugin: &str, key: &str) -> Option<String> {
        self.config.lock().unwrap().get(key).cloned()
    }

    fn supports(&self, _plugin: &str, _feature: &str) -> bool {
        false
    }

    fn permission_check(&self, _plugin: &str, id: u64, node: &str, _op: u8) -> bool {
        self.permissions
            .lock()
            .unwrap()
            .contains(&(id, node.to_string()))
    }

    fn player_position(&self, _plugin: &str, player: u64) -> Option<(String, f64, f64, f64)> {
        self.players
            .lock()
            .unwrap()
            .get(&player)
            .map(|(_, world, x, y, z)| (world.clone(), *x, *y, *z))
    }

    fn send_message(&self, _plugin: &str, player: u64, text: &str) {
        self.messages
            .lock()
            .unwrap()
            .push((player, text.to_string()));
    }

    fn region_upsert(&self, plugin: &str, region: &RegionInfo) {
        let mut region = region.clone();
        region.source = Some(plugin.to_string());
        self.regions.lock().unwrap().insert(region.id, region);
    }

    fn region_remove(&self, plugin: &str, id: u64) {
        let mut regions = self.regions.lock().unwrap();
        let owned = regions
            .get(&id)
            .is_some_and(|region| region.source.as_deref() == Some(plugin));
        if owned {
            regions.remove(&id);
        }
    }

    fn region_get(&self, _plugin: &str, id: u64) -> Option<RegionInfo> {
        self.regions.lock().unwrap().get(&id).cloned()
    }

    fn regions_overlapping(
        &self,
        _plugin: &str,
        world: &str,
        min: [i32; 3],
        max: [i32; 3],
    ) -> Vec<RegionInfo> {
        self.regions
            .lock()
            .unwrap()
            .values()
            .filter(|region| {
                same_world(&region.world, world)
                    && (0..3)
                        .all(|axis| region.min[axis] <= max[axis] && min[axis] <= region.max[axis])
            })
            .cloned()
            .collect()
    }
}

fn with_ctx<R>(backend: &Arc<Mock>, f: impl FnOnce(&mut NativeContext<'_>) -> R) -> R {
    let services = Arc::new(ServiceRegistry::new());
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let config = BTreeMap::new();
    let capabilities: Vec<String> = Vec::new();
    let mut ctx = NativeContext {
        id: PLUGIN_ID,
        capabilities: &capabilities,
        config: &config,
        services: &services,
        backend: &backend_dyn,
    };
    f(&mut ctx)
}

fn new_plugin(backend: &Arc<Mock>) -> Box<dyn NativePlugin> {
    let mut plugin = (registration().new)();
    with_ctx(backend, |ctx| plugin.init(ctx).unwrap());
    plugin
}

fn command(
    plugin: &mut Box<dyn NativePlugin>,
    backend: &Arc<Mock>,
    p: &Player,
    name: &str,
    args: &[&str],
) -> Vec<String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    with_ctx(backend, |ctx| {
        plugin.on_command(ctx, p, name, &args).unwrap().lines
    })
}

fn use_block(
    plugin: &mut Box<dyn NativePlugin>,
    backend: &Arc<Mock>,
    p: &Player,
    x: i32,
    z: i32,
    item: &str,
) -> Decision {
    with_ctx(backend, |ctx| {
        plugin.before_use_block(ctx, p, x, 64, z, item, "minecraft:stone")
    })
}

fn player(id: u64, name: &str) -> Player {
    Player {
        id,
        name: name.to_string(),
        operator: false,
    }
}

#[test]
fn claim_command_creates_a_chunk_and_inspects_it_next_time() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 15.0, 15.0);
    backend.grant_all(1);
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let lines = command(&mut plugin, &backend, &alice, "claim", &[]);
    assert!(lines[0].contains("Claim #1 created"), "{lines:?}");
    let claims = backend.claim_regions();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].min, [0, -64, 0]);
    assert_eq!(claims[0].max, [15, 383, 15]);
    assert_eq!(claims[0].priority, 10);
    assert_eq!(claims[0].owners, vec!["Alice".to_string()]);

    backend.clear_messages();
    let lines = command(&mut plugin, &backend, &alice, "claim", &[]);
    assert!(lines[0].contains("owned by you"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 1, "inspect must not claim");
}

#[test]
fn a_claim_can_cross_chunk_borders() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 15.0, 15.0);
    backend.grant_all(1);
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let lines = command(&mut plugin, &backend, &alice, "claim", &["1"]);
    assert!(lines[0].contains("Claim #1 created"), "{lines:?}");
    let claims = backend.claim_regions();
    assert_eq!(claims[0].min, [14, -64, 14]);
    assert_eq!(claims[0].max, [16, 383, 16]);
    for (x, z) in [(14, 14), (15, 15), (16, 16)] {
        let found = backend.regions_overlapping(PLUGIN_ID, OVERWORLD, [x, 64, z], [x, 64, z]);
        assert!(
            found.iter().any(|region| region.id == claims[0].id),
            "claim not found at {x},{z}"
        );
    }
}

#[test]
fn claiming_land_owned_by_another_player_is_refused() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.add_player(2, "Bob", 0.0, 0.0);
    backend.grant_all(1);
    backend.grant_all(2);
    let alice = player(1, "Alice");
    let bob = player(2, "Bob");
    let mut plugin = new_plugin(&backend);

    command(&mut plugin, &backend, &alice, "claim", &[]);
    assert_eq!(backend.claim_regions().len(), 1);

    let lines = command(&mut plugin, &backend, &bob, "claim", &[]);
    assert!(lines[0].contains("owned by Alice"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 1);

    let lines = command(&mut plugin, &backend, &bob, "claim", &["1"]);
    assert!(lines[0].contains("already claimed by Alice"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 1);
}

#[test]
fn a_worldguard_region_overlap_is_allowed_and_claims_keep_their_priority() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let mut config = BTreeMap::new();
    config.insert("priority".to_string(), "20".to_string());
    *backend.config.lock().unwrap() = config;
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let station = RegionInfo {
        id: 99,
        source: Some("regions".to_string()),
        world: OVERWORLD.to_string(),
        min: [-32, -64, -32],
        max: [31, 383, 31],
        priority: 100,
        flags: BTreeMap::new(),
        members: Vec::new(),
        owners: vec!["Staff".to_string()],
    };
    backend.add_region(station);

    let lines = command(&mut plugin, &backend, &alice, "claim", &[]);
    assert!(lines[0].contains("Claim #1 created"), "{lines:?}");
    let claims = backend.claim_regions();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].priority, 20);
    // The higher-priority WorldGuard region still wins the core check; the
    // claim was allowed to exist because claims and regions share the index.
    assert_eq!(
        backend.regions.lock().unwrap().get(&99).unwrap().priority,
        100
    );
}

#[test]
fn the_claim_count_limit_is_per_player() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let mut config = BTreeMap::new();
    config.insert("max-claims".to_string(), "1".to_string());
    *backend.config.lock().unwrap() = config;
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    command(&mut plugin, &backend, &alice, "claim", &["1"]);
    assert_eq!(backend.claim_regions().len(), 1);

    backend.add_player(1, "Alice", 100.0, 100.0);
    let lines = command(&mut plugin, &backend, &alice, "claim", &["1"]);
    assert!(lines[0].contains("Claim limit reached"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 1);
}

#[test]
fn an_oversized_claim_is_refused_with_the_shortfall() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let mut config = BTreeMap::new();
    config.insert("max-claim-size".to_string(), "100".to_string());
    config.insert("max-radius".to_string(), "10".to_string());
    *backend.config.lock().unwrap() = config;
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let lines = command(&mut plugin, &backend, &alice, "claim", &[]);
    assert!(lines[0].contains("256 blocks"), "{lines:?}");
    assert!(lines[0].contains("156 over"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 0);
}

#[test]
fn an_invalid_radius_is_refused() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let mut config = BTreeMap::new();
    config.insert("max-radius".to_string(), "8".to_string());
    *backend.config.lock().unwrap() = config;
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let lines = command(&mut plugin, &backend, &alice, "claim", &["abc"]);
    assert!(lines[0].contains("Invalid radius"), "{lines:?}");
    let lines = command(&mut plugin, &backend, &alice, "claim", &["-1"]);
    assert!(lines[0].contains("zero or more"), "{lines:?}");
    let lines = command(&mut plugin, &backend, &alice, "claim", &["9"]);
    assert!(lines[0].contains("too large"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 0);
}

#[test]
fn abandoning_a_claim_frees_it_and_its_region() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    command(&mut plugin, &backend, &alice, "claim", &[]);
    assert_eq!(backend.claim_regions().len(), 1);

    let lines = command(&mut plugin, &backend, &alice, "abandonclaim", &[]);
    assert!(lines[0].contains("abandoned"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 0);
    let lines = command(&mut plugin, &backend, &alice, "claimslist", &[]);
    assert!(lines[0].contains("no claims"), "{lines:?}");
}

#[test]
fn a_non_owner_cannot_abandon_or_claim_over_another_players_land() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.add_player(2, "Bob", 0.0, 0.0);
    backend.grant_all(1);
    backend.grant_all(2);
    let alice = player(1, "Alice");
    let bob = player(2, "Bob");
    let mut plugin = new_plugin(&backend);

    command(&mut plugin, &backend, &alice, "claim", &[]);
    let lines = command(&mut plugin, &backend, &bob, "abandonclaim", &[]);
    assert!(lines[0].contains("belongs to Alice"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 1);

    backend.clear_messages();
    let decision = use_block(&mut plugin, &backend, &bob, 0, 0, "minecraft:golden_shovel");
    assert!(matches!(decision, Decision::Cancel(_)));
    assert!(backend.messages().join("\n").contains("owned by Alice"));
    assert_eq!(backend.claim_regions().len(), 1);
}

#[test]
fn abandon_all_requires_a_second_confirmation() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    command(&mut plugin, &backend, &alice, "claim", &[]);
    backend.add_player(1, "Alice", 100.0, 100.0);
    command(&mut plugin, &backend, &alice, "claim", &[]);
    assert_eq!(backend.claim_regions().len(), 2);

    let lines = command(&mut plugin, &backend, &alice, "abandonallclaims", &[]);
    assert!(lines[0].contains("again"), "{lines:?}");
    assert!(lines[0].contains("2 claim"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 2, "not confirmed yet");

    let lines = command(&mut plugin, &backend, &alice, "abandonallclaims", &[]);
    assert!(lines[0].contains("Abandoned 2 claim"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 0);
}

#[test]
fn claims_survive_a_restart_and_are_replayed_into_the_index() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let alice = player(1, "Alice");
    {
        let mut first = new_plugin(&backend);
        command(&mut first, &backend, &alice, "claim", &[]);
        assert_eq!(backend.claim_regions().len(), 1);
    }

    let mut second = new_plugin(&backend);
    let lines = command(&mut second, &backend, &alice, "claimslist", &[]);
    assert!(lines[0].contains("1 claim"), "{lines:?}");
    assert!(lines[1].contains("(0, 0)..(15, 15)"), "{lines:?}");
    assert_eq!(
        backend.claim_regions().len(),
        1,
        "startup must replay exactly one region"
    );
}

#[test]
fn claims_list_shows_only_your_own_unless_you_are_a_moderator() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.add_player(2, "Bob", 1000.0, 1000.0);
    backend.grant_all(1);
    backend.grant_all(2);
    {
        let mut perms = backend.permissions.lock().unwrap();
        perms.remove(&(1, NODE_LIST_OTHERS.to_string()));
        perms.remove(&(1, NODE_ADMIN.to_string()));
    }
    let alice = player(1, "Alice");
    let bob = player(2, "Bob");
    let mut plugin = new_plugin(&backend);

    command(&mut plugin, &backend, &alice, "claim", &[]);
    command(&mut plugin, &backend, &bob, "claim", &[]);

    let lines = command(&mut plugin, &backend, &alice, "claimslist", &[]);
    let text = lines.join("\n");
    assert!(text.contains("(0, 0)..(15, 15)"), "{text}");
    assert!(!text.contains("(992, 992)"), "{text}");

    let lines = command(&mut plugin, &backend, &alice, "claimslist", &["Bob"]);
    assert!(lines[0].contains("permission"), "{lines:?}");

    let mut moderator = player(3, "Mod");
    moderator.operator = true;
    backend.grant_all(3);
    let lines = command(&mut plugin, &backend, &moderator, "claimslist", &["Bob"]);
    let text = lines.join("\n");
    assert!(text.contains("(992, 992)..(1007, 1007)"), "{text}");
    assert!(text.contains("moderator view"), "{text}");
}

#[test]
fn the_golden_shovel_claims_on_the_second_click_and_can_be_reset() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let decision = use_block(
        &mut plugin,
        &backend,
        &alice,
        10,
        10,
        "minecraft:golden_shovel",
    );
    assert!(matches!(decision, Decision::Cancel(_)));
    assert!(backend.last_message().contains("First corner"));
    assert_eq!(backend.claim_regions().len(), 0);

    use_block(
        &mut plugin,
        &backend,
        &alice,
        20,
        20,
        "minecraft:golden_shovel",
    );
    assert!(backend.last_message().contains("Claim #1 created"));
    let claims = backend.claim_regions();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].min, [10, -64, 10]);
    assert_eq!(claims[0].max, [20, 383, 20]);

    backend.clear_messages();
    use_block(
        &mut plugin,
        &backend,
        &alice,
        30,
        30,
        "minecraft:golden_shovel",
    );
    use_block(
        &mut plugin,
        &backend,
        &alice,
        30,
        30,
        "minecraft:golden_shovel",
    );
    assert!(backend.last_message().contains("cleared"));
    use_block(
        &mut plugin,
        &backend,
        &alice,
        40,
        40,
        "minecraft:golden_shovel",
    );
    assert!(backend.last_message().contains("First corner"));
    assert_eq!(backend.claim_regions().len(), 1, "reset must not claim");
    use_block(
        &mut plugin,
        &backend,
        &alice,
        40,
        40,
        "minecraft:golden_shovel",
    );
    assert!(backend.last_message().contains("cleared"));

    backend.clear_messages();
    use_block(
        &mut plugin,
        &backend,
        &alice,
        50,
        50,
        "minecraft:golden_shovel",
    );
    use_block(
        &mut plugin,
        &backend,
        &alice,
        15,
        15,
        "minecraft:golden_shovel",
    );
    assert!(backend.messages().join("\n").contains("owned by you"));
    use_block(
        &mut plugin,
        &backend,
        &alice,
        60,
        60,
        "minecraft:golden_shovel",
    );
    assert!(backend.last_message().contains("First corner"));
    assert_eq!(backend.claim_regions().len(), 1);
}

#[test]
fn other_items_and_entity_or_air_use_do_not_claim() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    backend.grant_all(1);
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let decision = use_block(&mut plugin, &backend, &alice, 5, 5, "minecraft:stone");
    assert!(matches!(decision, Decision::Allow));
    assert_eq!(backend.claim_regions().len(), 0);

    let decision = with_ctx(&backend, |ctx| {
        plugin.before_use_item(ctx, &alice, "minecraft:golden_shovel")
    });
    assert!(matches!(decision, Decision::Allow));
    assert_eq!(backend.claim_regions().len(), 0);
    assert!(backend.messages().is_empty());

    use_block(
        &mut plugin,
        &backend,
        &alice,
        5,
        5,
        "minecraft:golden_shovel",
    );
    assert!(backend.last_message().contains("First corner"));
    assert_eq!(backend.claim_regions().len(), 0);
}

#[test]
fn claims_work_near_the_world_border_and_with_negative_coordinates() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 29_999_984.0, 29_999_984.0);
    backend.grant_all(1);
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let lines = command(&mut plugin, &backend, &alice, "claim", &["1"]);
    assert!(lines[0].contains("created"), "{lines:?}");
    let claims = backend.claim_regions();
    assert_eq!(claims[0].min[0], 29_999_983);
    assert_eq!(claims[0].max[0], 29_999_985);

    backend.add_player(1, "Alice", -29_999_984.0, -29_999_984.0);
    let lines = command(&mut plugin, &backend, &alice, "claim", &["1"]);
    assert!(lines[0].contains("created"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 2);
}

#[test]
fn a_player_without_the_node_cannot_claim() {
    let backend = Arc::new(Mock::default());
    backend.add_player(1, "Alice", 0.0, 0.0);
    let alice = player(1, "Alice");
    let mut plugin = new_plugin(&backend);

    let lines = command(&mut plugin, &backend, &alice, "claim", &[]);
    assert!(lines[0].contains("permission"), "{lines:?}");
    assert_eq!(backend.claim_regions().len(), 0);

    let decision = use_block(
        &mut plugin,
        &backend,
        &alice,
        5,
        5,
        "minecraft:golden_shovel",
    );
    assert!(matches!(decision, Decision::Allow));
    assert!(backend.last_message().contains("permission"));
}
