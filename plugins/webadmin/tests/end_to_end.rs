//! End-to-end WEB2: a live world, edited by two players, served through the
//! public API — map tiles, the time slider, per-block history and per-player
//! history — with no sockets and no private helpers.

use std::collections::BTreeMap;
use std::sync::Arc;

use aether_api::World;
use aether_webadmin::http::actor_hex;
use aether_webadmin::{Request, WebAdmin, WebAdminConfig};
use aether_world::journal::ActorId;
use aether_world::registry::ids;
use aether_world::MemStore;
use aether_worldgen::FlatGenerator;

const ALICE: ActorId = ActorId(0xA11CE);
const BOB: ActorId = ActorId(0xB0B);

fn get(admin: &WebAdmin, path: &str, q: &[(&str, &str)]) -> (u16, Vec<u8>) {
    let mut query = BTreeMap::new();
    for (k, v) in q {
        query.insert(k.to_string(), v.to_string());
    }
    let res = admin.handle(&Request {
        method: "GET".to_string(),
        path: path.to_string(),
        query,
        token: Some("test".to_string()),
        ..Request::default()
    });
    (res.status, res.body)
}

fn json(admin: &WebAdmin, path: &str, q: &[(&str, &str)]) -> String {
    let (status, body) = get(admin, path, q);
    assert_eq!(status, 200, "route {path}");
    String::from_utf8(body).unwrap()
}

#[test]
fn map_time_machine_and_history_end_to_end() {
    let world = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
    // Baseline: grass at y=3. Alice replaces it with stone, Bob then with dirt.
    let props_stone = world.props_of(ids::STONE);
    let props_dirt = world.props_of(ids::DIRT);
    let s1 = world
        .set_block_by(ALICE, 0, 3, 0, ids::STONE, props_stone)
        .unwrap();
    let s2 = world
        .set_block_by(BOB, 0, 3, 0, ids::DIRT, props_dirt)
        .unwrap();
    assert!(s2 > s1);

    let cfg = WebAdminConfig {
        tile_px: 4,
        y_min: 0,
        y_max: 8,
        token: Some("test".to_string()),
        history_limit: 100,
    };
    let admin = WebAdmin::with_config(world.clone(), world, cfg);

    // Live map shows the latest block (dirt).
    let live = get(&admin, "/tile", &[("zoom", "0"), ("tx", "0"), ("tz", "0")]).1;
    // The first pixel, three ways through time.
    let pixel = |body: &[u8]| [body[0], body[1], body[2], body[3]];
    assert_eq!(pixel(&live), [134, 96, 67, 255], "dirt is the live surface");

    let at_start = get(
        &admin,
        "/tile_at",
        &[("zoom", "0"), ("tx", "0"), ("tz", "0"), ("at", "0")],
    )
    .1;
    assert_eq!(
        pixel(&at_start),
        [96, 158, 64, 255],
        "worldgen grass at seq 0"
    );

    let at_alice = get(
        &admin,
        "/tile_at",
        &[
            ("zoom", "0"),
            ("tx", "0"),
            ("tz", "0"),
            ("at", &s1.to_string()),
        ],
    )
    .1;
    assert_eq!(pixel(&at_alice), [125, 125, 125, 255], "stone after Alice");

    // Click-the-block history: two edits, then order and authors.
    let history = json(&admin, "/history", &[("x", "0"), ("y", "3"), ("z", "0")]);
    assert!(history.contains(&actor_hex(ALICE)), "{history}");
    assert!(history.contains(&actor_hex(BOB)), "{history}");
    assert!(history.contains("grass_block"), "{history}");

    // Per-player history: Alice's one action, jumping to the map is implied by
    // the coordinates in the response.
    let alice = json(
        &admin,
        "/player",
        &[
            ("id", &actor_hex(ALICE)),
            ("x", "0"),
            ("z", "0"),
            ("r", "8"),
        ],
    );
    assert!(alice.contains("\"action\":\"change\""), "{alice}");
    assert!(
        !alice.contains(&actor_hex(BOB)),
        "Bob is filtered out: {alice}"
    );

    // Area/time listing carries the coordinates for the map jump.
    let changes = json(&admin, "/changes", &[("x", "0"), ("z", "0"), ("r", "8")]);
    assert!(changes.contains("\"x\":0"), "{changes}");

    // Head advances with every edit.
    let head = json(&admin, "/head", &[]);
    assert!(head.contains(&format!("\"head\":{}", s2 + 1)), "{head}");

    // The tile cache invalidates and rebuilds from the live world.
    admin.map().invalidate(0, 0);
    let rebuilt = get(&admin, "/tile", &[("zoom", "0"), ("tx", "0"), ("tz", "0")]).1;
    assert_eq!(pixel(&rebuilt), pixel(&live));
}

#[test]
fn overlay_is_empty_by_default() {
    let world = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
    let cfg = WebAdminConfig {
        token: Some("test".to_string()),
        ..WebAdminConfig::default()
    };
    let admin = WebAdmin::with_config(world.clone(), world, cfg);
    let (status, body) = get(&admin, "/overlay", &[]);
    assert_eq!(status, 200);
    assert_eq!(
        String::from_utf8(body).unwrap(),
        "{\"players\":[],\"entities\":[]}"
    );
}

#[test]
fn world_coerces_to_both_read_views() {
    // One `Arc<World>` stands in for both views: the blanket impls are what
    // let the server pass a single handle to `WebAdmin`.
    let world = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
    let map: Arc<dyn aether_webadmin::MapSource> = world.clone();
    let history: Arc<dyn aether_webadmin::HistorySource> = world;
    assert_eq!(map.y_max(), 319);
    assert_eq!(history.head(), 1);
    assert!(history.column_events(0, 0).unwrap().is_empty());
}

/// A minimal live-ops service: the WEB1 routes render it through the same
/// router the map uses.
struct MockOps;

impl aether_ops::OpsService for MockOps {
    fn dashboard(&self) -> aether_ops::Dashboard {
        aether_ops::Dashboard {
            tps: 20.0,
            mspt: 50.0,
            mspt_max: 50.0,
            players_online: 1,
            players_max: 20,
            ticks: 100,
            uptime_secs: 5,
            memory_used_mib: 128,
            memory_total_mib: 4096,
            chunk_sends: 7,
        }
    }
    fn regions(&self, _limit: usize) -> Vec<aether_ops::RegionStat> {
        vec![aether_ops::RegionStat {
            rx: 0,
            rz: 0,
            entities: 3,
            block_entities: 1,
            block_updates: 0,
            redstone_updates: 0,
            chunk_sends: 8,
            cost: 7.0,
            samples: 1,
        }]
    }
    fn players(&self) -> Vec<aether_ops::PlayerEntry> {
        vec![aether_ops::PlayerEntry {
            uuid: "a".repeat(32),
            name: "Alice".to_string(),
            online: true,
            ip: Some("127.0.0.1".to_string()),
            first_seen: 0,
            last_seen: 0,
            playtime_secs: 5,
        }]
    }
    fn profile(&self, id: &str) -> Option<aether_ops::PlayerProfile> {
        (id == "Alice" || id.len() == 32).then(|| aether_ops::PlayerProfile {
            entry: self.players().into_iter().next().unwrap(),
            ips: vec!["127.0.0.1".to_string()],
            session_secs: 5,
            position: Some((1.0, 64.0, 2.0)),
            health: Some(20.0),
            food: Some(20),
            game_mode: Some("survival".to_string()),
        })
    }
    fn inventory(&self, id: &str) -> Option<aether_ops::InventoryView> {
        (id == "Alice" || id.len() == 32).then(|| aether_ops::InventoryView {
            uuid: "a".repeat(32),
            name: "Alice".to_string(),
            online: true,
            held_slot: 0,
            slots: vec![aether_ops::ItemView {
                slot: 36,
                item: "minecraft:diamond".to_string(),
                count: 5,
                damage: 0,
                potion: None,
            }],
            armor: Vec::new(),
            ender: Vec::new(),
        })
    }
}

#[test]
fn live_ops_routes_share_the_router() {
    let world = Arc::new(World::new(MemStore::new(), FlatGenerator::classic()));
    let cfg = WebAdminConfig {
        token: Some("test".to_string()),
        ..WebAdminConfig::default()
    };
    let ops = aether_ops::OpsHandle(Arc::new(MockOps));
    let admin = WebAdmin::with_config(world.clone(), world, cfg).with_ops(ops);

    let dashboard = json(&admin, "/", &[]);
    assert!(dashboard.contains("Dashboard"), "{dashboard}");
    assert!(dashboard.contains("20.0"), "{dashboard}");

    let regions = json(&admin, "/regions", &[]);
    assert!(regions.contains("Heaviest regions"), "{regions}");

    let players = json(&admin, "/players", &[]);
    assert!(players.contains("Alice"), "{players}");

    // The JSON API and the inventory page.
    let api = json(&admin, "/api/dashboard", &[]);
    assert!(api.contains("\"players_online\":1"), "{api}");
    let inv = json(&admin, "/inventory", &[("id", "Alice")]);
    assert!(inv.contains("minecraft:diamond"), "{inv}");
}
