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
        token: None,
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
        token: None,
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
    let admin = WebAdmin::new(world.clone(), world);
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
