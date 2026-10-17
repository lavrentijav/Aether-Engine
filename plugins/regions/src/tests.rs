use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use aether_plugin::service::ServiceRegistry;
use aether_plugin::{HostBackend, LogLevel};
use aether_region::{same_world, Action, Actor, RegionIndex, RegionStore, Verdict};

#[derive(Default)]
struct TestBackend {
    kv: Mutex<HashMap<(String, String), Vec<u8>>>,
    messages: Mutex<Vec<(u64, String)>>,
    logs: Mutex<Vec<String>>,
    held: Mutex<Option<String>>,
    permissions: AtomicBool,
    fail_get: AtomicBool,
    fail_put: AtomicBool,
    position: Mutex<Option<(String, f64, f64, f64)>>,
}

impl TestBackend {
    fn permissive() -> Self {
        let backend = TestBackend::default();
        backend.permissions.store(true, Ordering::SeqCst);
        backend
    }

    fn set_held(&self, item: Option<&str>) {
        *self.held.lock().unwrap() = item.map(str::to_string);
    }

    fn messages(&self) -> Vec<(u64, String)> {
        self.messages.lock().unwrap().clone()
    }

    fn set_position(&self, world: &str, x: f64, y: f64, z: f64) {
        *self.position.lock().unwrap() = Some((world.to_string(), x, y, z));
    }
}

impl HostBackend for TestBackend {
    fn kv_get(&self, plugin: &str, key: &str) -> Option<Vec<u8>> {
        self.kv
            .lock()
            .unwrap()
            .get(&(plugin.to_string(), key.to_string()))
            .cloned()
    }
    fn kv_put(&self, plugin: &str, key: &str, value: &[u8]) {
        self.kv
            .lock()
            .unwrap()
            .insert((plugin.to_string(), key.to_string()), value.to_vec());
    }
    fn kv_delete(&self, plugin: &str, key: &str) {
        self.kv
            .lock()
            .unwrap()
            .remove(&(plugin.to_string(), key.to_string()));
    }
    fn kv_try_get(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
        if self.fail_get.load(Ordering::SeqCst) {
            return Err("injected read failure".to_string());
        }
        Ok(self.kv_get(plugin, key))
    }
    fn kv_try_put(&self, plugin: &str, key: &str, value: &[u8]) -> Result<(), String> {
        if self.fail_put.load(Ordering::SeqCst) {
            return Err("injected write failure".to_string());
        }
        self.kv_put(plugin, key, value);
        Ok(())
    }
    fn kv_list(&self, plugin: &str, prefix: &str) -> Vec<String> {
        self.kv
            .lock()
            .unwrap()
            .keys()
            .filter(|(p, k)| p == plugin && k.starts_with(prefix))
            .map(|(_, k)| k.clone())
            .collect()
    }
    fn log(&self, _plugin: &str, _level: LogLevel, message: &str) {
        self.logs.lock().unwrap().push(message.to_string());
    }
    fn config(&self, _plugin: &str, _key: &str) -> Option<String> {
        None
    }
    fn supports(&self, _plugin: &str, _feature: &str) -> bool {
        true
    }
    fn held_item(&self, _plugin: &str, _player: u64) -> Option<String> {
        self.held.lock().unwrap().clone()
    }
    fn send_message(&self, _plugin: &str, player: u64, text: &str) {
        self.messages
            .lock()
            .unwrap()
            .push((player, text.to_string()));
    }
    fn permission_check(&self, _plugin: &str, _id: u64, _node: &str, _op: u8) -> bool {
        self.permissions.load(Ordering::SeqCst)
    }
    fn player_position(&self, _plugin: &str, _player: u64) -> Option<(String, f64, f64, f64)> {
        self.position.lock().unwrap().clone()
    }
}

fn player(id: u64, name: &str, operator: bool) -> Player {
    Player {
        id,
        name: name.to_string(),
        operator,
    }
}

fn ctx<'a>(
    backend: &'a Arc<dyn HostBackend>,
    services: &'a Arc<ServiceRegistry>,
    config: &'a BTreeMap<String, String>,
) -> NativeContext<'a> {
    NativeContext {
        id: ID,
        capabilities: &[],
        config,
        services,
        backend,
    }
}

struct Fixture {
    plugin: RegionsPlugin,
    backend: Arc<TestBackend>,
    backend_dyn: Arc<dyn HostBackend>,
    store: Arc<RegionStore>,
    services: Arc<ServiceRegistry>,
    config: BTreeMap<String, String>,
}

impl Fixture {
    fn new() -> Self {
        let backend = Arc::new(TestBackend::permissive());
        let backend_dyn: Arc<dyn HostBackend> = backend.clone();
        let store = Arc::new(RegionStore::new());
        let mut plugin = RegionsPlugin::with_store(Arc::clone(&store));
        let services = Arc::new(ServiceRegistry::new());
        let config = BTreeMap::new();
        {
            let mut ctx = ctx(&backend_dyn, &services, &config);
            plugin.init(&mut ctx).unwrap();
        }
        Fixture {
            plugin,
            backend,
            backend_dyn,
            store,
            services,
            config,
        }
    }

    fn restart(&mut self) {
        let store = Arc::new(RegionStore::new());
        let mut plugin = RegionsPlugin::with_store(Arc::clone(&store));
        {
            let backend = Arc::clone(&self.backend_dyn);
            let services = Arc::clone(&self.services);
            let config = self.config.clone();
            let mut ctx = ctx(&backend, &services, &config);
            plugin.init(&mut ctx).unwrap();
        }
        // The old store must not satisfy the post-restart assertions: the new
        // plugin rebuilt its own index from KV.
        self.store = store;
        self.plugin = plugin;
    }

    fn select(&mut self, a: BlockPos, b: BlockPos) {
        self.plugin.regions.set_point(1, true, a);
        self.plugin.regions.set_point(1, false, b);
    }

    fn command(&mut self, p: &Player, args: &[&str]) -> Vec<String> {
        let backend = Arc::clone(&self.backend_dyn);
        let services = Arc::clone(&self.services);
        let config = self.config.clone();
        let mut ctx = ctx(&backend, &services, &config);
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.plugin
            .on_command(&mut ctx, p, COMMAND_SHORT, &args)
            .expect("regions answers /rg")
            .lines
    }

    fn define(&mut self, p: &Player, name: &str, a: (i32, i32, i32), b: (i32, i32, i32)) {
        self.select(BlockPos::new(a.0, a.1, a.2), BlockPos::new(b.0, b.1, b.2));
        let lines = self.command(p, &["define", name]);
        assert!(
            lines.iter().any(|l| l.contains("Created region")),
            "{lines:?}"
        );
    }
}

fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn count_at(store: &RegionStore, p: BlockPos) -> usize {
    store.snapshot().overlapping(p).count()
}

#[test]
fn define_spans_chunk_borders_and_uses_the_one_core_index() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", false);
    // 14..=17 crosses the x=16 and z=16 seams: four chunk columns.
    f.define(&owner, "spawn", (14, 0, 14), (17, 80, 17));

    let index = f.store.snapshot();
    assert_eq!(
        index.bucket_count(),
        4,
        "a border-spanning cuboid is in every touched chunk"
    );
    for p in [
        pos(14, 64, 14),
        pos(17, 64, 17),
        pos(14, 64, 17),
        pos(17, 64, 14),
    ] {
        assert!(index.overlapping(p).any(|r| r.id == RegionId(1)), "{p:?}");
    }

    // The plugin holds the *same* store the hooks read, not a copy.
    assert!(Arc::ptr_eq(&f.plugin.regions.store, &f.store));

    // The core check, not a plugin-local copy, answers: the owner may build,
    // a stranger may not.
    assert_eq!(
        index.check(
            OVERWORLD,
            pos(15, 64, 15),
            Action::Break,
            &Actor::new("Steve")
        ),
        Verdict::Allow
    );
    assert_eq!(
        index.check(
            OVERWORLD,
            pos(15, 64, 15),
            Action::Break,
            &Actor::new("Alex")
        ),
        Verdict::Deny
    );
}

#[test]
fn an_invalid_id_errors_and_nothing_is_created() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", true);
    f.select(pos(0, 0, 0), pos(15, 64, 15));
    let lines = f.command(&owner, &["define", "bad id"]);
    assert!(
        lines.iter().any(|l| l.contains("invalid region id")),
        "{lines:?}"
    );
    assert!(f.plugin.regions.state.regions.is_empty());
    assert_eq!(f.store.snapshot().len(), 0);
}

#[test]
fn a_duplicate_id_errors() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", true);
    f.define(&owner, "spawn", (0, 0, 0), (15, 64, 15));
    f.select(pos(32, 0, 32), pos(47, 64, 47));
    let lines = f.command(&owner, &["define", "spawn"]);
    assert!(
        lines.iter().any(|l| l.contains("already exists")),
        "{lines:?}"
    );
    assert_eq!(f.store.snapshot().len(), 1);
    // The failed define did not consume the selection or move the bounds.
    assert_eq!(
        f.store.snapshot().get(RegionId(1)).unwrap().max,
        pos(15, 64, 15)
    );
}

#[test]
fn redefine_moves_bounds_atomically() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", true);
    f.define(&owner, "spawn", (0, 0, 0), (15, 64, 15));

    let before = f.store.snapshot();
    f.select(pos(32, 0, 32), pos(47, 64, 47));
    let lines = f.command(&owner, &["redefine", "spawn"]);
    assert!(
        lines.iter().any(|l| l.contains("Moved region")),
        "{lines:?}"
    );

    let after = f.store.snapshot();
    // The snapshot a caller already holds keeps the old, whole box.
    assert_eq!(before.len(), 1);
    assert_eq!(before.get(RegionId(1)).unwrap().max, pos(15, 64, 15));
    // The fresh snapshot has the new box, and only one region exists.
    assert_eq!(after.len(), 1);
    assert_eq!(after.get(RegionId(1)).unwrap().min, pos(32, 0, 32));
    assert_eq!(after.get(RegionId(1)).unwrap().max, pos(47, 64, 47));
    // Neither a half-old nor a half-new box is ever visible.
    assert!(after.overlapping(pos(8, 64, 8)).next().is_none());
    assert!(after.overlapping(pos(40, 64, 40)).next().is_some());
}

#[test]
fn list_shows_regions_per_world() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", true);
    f.define(&owner, "spawn", (0, 0, 0), (15, 64, 15));
    f.plugin.regions.state.regions.insert(
        "nether-hub".to_string(),
        StoredRegion {
            id: 2,
            name: "nether-hub".to_string(),
            world: "the_nether".to_string(),
            min: [0, 0, 0],
            max: [7, 7, 7],
            priority: 3,
            owners: BTreeSet::new(),
            members: BTreeSet::new(),
            flags: BTreeMap::new(),
        },
    );
    f.plugin.regions.rebuild();

    let lines = f.command(&owner, &["list"]);
    let text = lines.join("\n");
    assert!(
        text.contains("Regions in minecraft:overworld (1):"),
        "{text}"
    );
    assert!(text.contains("Regions in the_nether (1):"), "{text}");
    assert!(text.contains("spawn"), "{text}");
    assert!(text.contains("nether-hub"), "{text}");

    // The optional player filter keeps only the regions that player owns.
    let filtered = f.command(&owner, &["list", "Steve"]).join("\n");
    assert!(filtered.contains("spawn"), "{filtered}");
    assert!(!filtered.contains("nether-hub"), "{filtered}");
}

#[test]
fn remove_takes_effect_at_the_next_safe_point() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", true);
    f.define(&owner, "spawn", (0, 0, 0), (15, 64, 15));

    let before = f.store.snapshot();
    let lines = f.command(&owner, &["remove", "spawn"]);
    assert!(
        lines.iter().any(|l| l.contains("Removed region")),
        "{lines:?}"
    );
    // A caller still holding the old snapshot keeps answering from it...
    assert_eq!(before.overlapping(pos(8, 64, 8)).count(), 1);
    // ...the next snapshot no longer sees the region.
    assert_eq!(count_at(&f.store, pos(8, 64, 8)), 0);
    assert!(f.plugin.regions.by_name("spawn").is_none());
}

#[test]
fn a_region_without_an_owner_is_not_editable_by_a_non_op() {
    let mut f = Fixture::new();
    f.plugin.regions.state.regions.insert(
        "wild".to_string(),
        StoredRegion {
            id: 7,
            name: "wild".to_string(),
            world: OVERWORLD.to_string(),
            min: [0, 0, 0],
            max: [15, 64, 15],
            priority: 0,
            owners: BTreeSet::new(),
            members: BTreeSet::new(),
            flags: BTreeMap::new(),
        },
    );
    f.plugin.regions.rebuild();

    let guest = player(2, "Guest", false);
    f.select(pos(32, 0, 32), pos(47, 64, 47));
    let backend = Arc::clone(&f.backend_dyn);
    let services = Arc::clone(&f.services);
    let config = f.config.clone();
    let mut ctx = ctx(&backend, &services, &config);
    let err = f
        .plugin
        .regions
        .redefine(
            &mut ctx,
            Caller {
                name: &guest.name,
                op: false,
            },
            "wild",
            pos(32, 0, 32),
            pos(47, 64, 47),
        )
        .expect_err("a non-op may not move an unowned region");
    assert!(err.contains("do not own"), "{err}");
    assert_eq!(
        f.store.snapshot().get(RegionId(7)).unwrap().max,
        pos(15, 64, 15)
    );

    // Through the command the same refusal is a readable line.
    f.plugin.regions.set_point(guest.id, true, pos(32, 0, 32));
    f.plugin.regions.set_point(guest.id, false, pos(47, 64, 47));
    let lines = f.command(&guest, &["redefine", "wild"]);
    assert!(lines.iter().any(|l| l.contains("do not own")), "{lines:?}");

    // An operator may.
    let op = player(3, "Admin", true);
    f.plugin.regions.set_point(op.id, true, pos(32, 0, 32));
    f.plugin.regions.set_point(op.id, false, pos(47, 64, 47));
    let lines = f.command(&op, &["redefine", "wild"]);
    assert!(
        lines.iter().any(|l| l.contains("Moved region")),
        "{lines:?}"
    );
    assert_eq!(
        f.store.snapshot().get(RegionId(7)).unwrap().max,
        pos(47, 64, 47)
    );
}

#[test]
fn state_survives_a_restart() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", true);
    f.define(&owner, "spawn", (0, 0, 0), (15, 64, 15));
    let lines = f.command(&owner, &["priority", "spawn", "9"]);
    assert!(
        lines.iter().any(|l| l.contains("priority is now 9")),
        "{lines:?}"
    );

    // A fresh plugin instance over the same KV: load + rebuild the core store.
    f.restart();
    assert_eq!(f.plugin.regions.state.regions.len(), 1);
    let loaded = f.plugin.regions.by_name("spawn").expect("loaded");
    assert_eq!(loaded.priority, 9);
    assert!(loaded.owners.contains("Steve"));
    assert_eq!(count_at(&f.store, pos(8, 64, 8)), 1);
    // The stable numeric id and next id survived too.
    assert_eq!(loaded.id, 1);
    f.select(pos(32, 0, 32), pos(47, 64, 47));
    let lines = f.command(&owner, &["define", "market"]);
    assert!(
        lines.iter().any(|l| l.contains("Created region")),
        "{lines:?}"
    );
    assert_eq!(f.plugin.regions.by_name("market").unwrap().id, 2);
}

#[test]
fn priorities_and_equal_priority_ties_are_deterministic() {
    let mut f = Fixture::new();
    // Two overlapping regions; the lower id has an explicit build deny, the
    // higher id an allow. At equal priority the lower id decides.
    for (id, name, build) in [(1, "low", false), (2, "high", true)] {
        let mut flags = BTreeMap::new();
        flags.insert("build".to_string(), build);
        f.plugin.regions.state.regions.insert(
            name.to_string(),
            StoredRegion {
                id,
                name: name.to_string(),
                world: OVERWORLD.to_string(),
                min: [0, 0, 0],
                max: [15, 64, 15],
                priority: 0,
                owners: BTreeSet::new(),
                members: BTreeSet::new(),
                flags,
            },
        );
    }
    f.plugin.regions.rebuild();
    let owner = player(1, "Steve", true);
    assert_eq!(
        f.store
            .check(OVERWORLD, pos(8, 64, 8), Action::Break, &Actor::new("Alex")),
        Verdict::Deny,
        "equal priority: id 1 decides before id 2"
    );

    // Raising the allowing region above the deny changes the answer.
    let lines = f.command(&owner, &["priority", "high", "5"]);
    assert!(lines.iter().any(|l| l.contains("now 5")), "{lines:?}");
    assert_eq!(
        f.store
            .check(OVERWORLD, pos(8, 64, 8), Action::Break, &Actor::new("Alex")),
        Verdict::Allow
    );

    // ...and dropping it below restores the deterministic deny.
    let lines = f.command(&owner, &["priority", "high", "-5"]);
    assert!(lines.iter().any(|l| l.contains("now -5")), "{lines:?}");
    assert_eq!(
        f.store
            .check(OVERWORLD, pos(8, 64, 8), Action::Break, &Actor::new("Alex")),
        Verdict::Deny
    );
}

#[test]
fn the_internal_wand_sets_both_points_and_cancels_the_click() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", false);
    f.backend.set_held(Some("minecraft:golden_axe"));

    {
        let backend = Arc::clone(&f.backend_dyn);
        let services = Arc::clone(&f.services);
        let config = f.config.clone();
        let mut ctx = ctx(&backend, &services, &config);
        let decision = f
            .plugin
            .before_block_dig(&mut ctx, &owner, 1, 2, 3, "minecraft:stone");
        assert!(matches!(decision, Decision::Cancel(reason) if reason.is_empty()));
        let decision = f
            .plugin
            .before_block_interact(&mut ctx, &owner, 4, 5, 6, "minecraft:dirt");
        assert!(matches!(decision, Decision::Cancel(reason) if reason.is_empty()));
    }
    let messages = f.backend.messages();
    assert!(messages.iter().any(|(_, m)| m.contains("First position")));
    assert!(messages.iter().any(|(_, m)| m.contains("Second position")));

    let lines = f.command(&owner, &["define", "wand"]);
    assert!(
        lines.iter().any(|l| l.contains("Created region")),
        "{lines:?}"
    );
    let region = f.plugin.regions.by_name("wand").unwrap();
    assert_eq!(region.min, [1, 2, 3]);
    assert_eq!(region.max, [4, 5, 6]);
    assert!(
        f.plugin.regions.selection(owner.id).is_none(),
        "a successful define clears the selection"
    );

    // Any other item leaves the click alone and starts no selection.
    f.backend.set_held(Some("minecraft:stone"));
    let backend = Arc::clone(&f.backend_dyn);
    let services = Arc::clone(&f.services);
    let config = f.config.clone();
    let mut ctx = ctx(&backend, &services, &config);
    let decision = f
        .plugin
        .before_block_dig(&mut ctx, &owner, 10, 10, 10, "minecraft:stone");
    assert!(matches!(decision, Decision::Allow));
}

#[test]
fn info_reports_a_region_by_name_and_missing_ones() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", true);
    f.define(&owner, "spawn", (0, 0, 0), (15, 64, 15));
    let lines = f.command(&owner, &["info", "spawn"]);
    let text = lines.join("\n");
    assert!(text.contains("Region `spawn`"), "{text}");
    assert!(text.contains("owners: Steve"), "{text}");

    let lines = f.command(&owner, &["info", "nope"]);
    assert!(
        lines.iter().any(|l| l.contains("does not exist")),
        "{lines:?}"
    );
}

#[test]
fn the_manifest_declares_the_native_tier_and_commands() {
    let manifest = manifest();
    assert_eq!(manifest.tier, PluginTier::Native);
    assert_eq!(manifest.id, ID);
    assert!(manifest.subscribes("before-block-interact"));
    assert!(manifest.serves_command(COMMAND_SHORT));
    assert!(manifest.serves_command(COMMAND_LONG));
    // The plugin builds the same index type the core check does.
    let index = RegionIndex::from_regions([]);
    assert!(index.is_empty());
}

#[test]
fn cancel_and_leave_clear_the_selection() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", false);
    f.select(pos(0, 0, 0), pos(1, 1, 1));
    let lines = f.command(&owner, &["cancel"]);
    assert!(lines.iter().any(|l| l.contains("cleared")), "{lines:?}");
    assert!(f.plugin.regions.selection(owner.id).is_none());

    f.select(pos(0, 0, 0), pos(1, 1, 1));
    let backend = Arc::clone(&f.backend_dyn);
    let services = Arc::clone(&f.services);
    let config = f.config.clone();
    let mut ctx = ctx(&backend, &services, &config);
    f.plugin.after_player_leave(&mut ctx, &owner);
    assert!(
        f.plugin.regions.selection(owner.id).is_none(),
        "a reused entity id must not inherit a selection"
    );
}

#[test]
fn define_uses_the_players_world_and_redefine_keeps_it() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", false);
    f.backend.set_held(Some("minecraft:golden_axe"));
    f.backend.set_position("the_nether", 10.0, 64.0, 10.0);
    f.select(pos(0, 0, 0), pos(15, 64, 15));
    let lines = f.command(&owner, &["define", "hub"]);
    assert!(
        lines.iter().any(|l| l.contains("Created region")),
        "{lines:?}"
    );
    assert_eq!(f.plugin.regions.by_name("hub").unwrap().world, "the_nether");
    assert!(f
        .store
        .snapshot()
        .overlapping(pos(8, 8, 8))
        .any(|r| same_world(&r.world, "the_nether")));

    // A redefine from another dimension must not teleport the region.
    f.backend.set_position(OVERWORLD, 100.0, 64.0, 100.0);
    f.select(pos(32, 0, 32), pos(47, 64, 47));
    let lines = f.command(&owner, &["redefine", "hub"]);
    assert!(
        lines.iter().any(|l| l.contains("Moved region")),
        "{lines:?}"
    );
    let moved = f.plugin.regions.by_name("hub").unwrap();
    assert_eq!(moved.world, "the_nether");
    assert_eq!(moved.min, [32, 0, 32]);
    assert_eq!(moved.max, [47, 64, 47]);
}

#[test]
fn a_failed_kv_write_does_not_change_state_or_the_index() {
    let mut f = Fixture::new();
    let owner = player(1, "Steve", true);
    f.backend.fail_put.store(true, Ordering::SeqCst);
    f.select(pos(0, 0, 0), pos(15, 64, 15));
    let lines = f.command(&owner, &["define", "spawn"]);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("cannot write region state")),
        "{lines:?}"
    );
    assert!(f.plugin.regions.by_name("spawn").is_none());
    assert_eq!(f.store.snapshot().len(), 0);

    // Once the store recovers the same command succeeds and persists.
    f.backend.fail_put.store(false, Ordering::SeqCst);
    let lines = f.command(&owner, &["define", "spawn"]);
    assert!(
        lines.iter().any(|l| l.contains("Created region")),
        "{lines:?}"
    );
    assert_eq!(f.store.snapshot().len(), 1);
}

#[test]
fn an_unreadable_kv_refuses_the_load_instead_of_starting_empty() {
    let backend = Arc::new(TestBackend::permissive());
    // A corrupt payload must not be replaced by an empty set.
    backend.kv.lock().unwrap().insert(
        (ID.to_string(), KV_REGIONS.to_string()),
        b"not json".to_vec(),
    );
    let store = Arc::new(RegionStore::new());
    let mut plugin = RegionsPlugin::with_store(Arc::clone(&store));
    let services = Arc::new(ServiceRegistry::new());
    let config = BTreeMap::new();
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut context = ctx(&backend_dyn, &services, &config);
    let err = plugin
        .init(&mut context)
        .expect_err("corrupt state refuses the load");
    assert!(err.contains("unreadable"), "{err}");
    // The plugin loaded nothing and, crucially, wrote nothing over the bytes.
    assert_eq!(
        backend
            .kv_get(ID, KV_REGIONS)
            .expect("the original bytes are still there"),
        b"not json"
    );

    // A storage read error is also a refusal, not an empty rebuild.
    let backend = Arc::new(TestBackend::permissive());
    backend.fail_get.store(true, Ordering::SeqCst);
    let store = Arc::new(RegionStore::new());
    let mut plugin = RegionsPlugin::with_store(Arc::clone(&store));
    let backend_dyn: Arc<dyn HostBackend> = backend.clone();
    let mut context = ctx(&backend_dyn, &services, &config);
    let err = plugin
        .init(&mut context)
        .expect_err("a read error refuses the load");
    assert!(err.contains("cannot read region state"), "{err}");
}

#[test]
fn the_top_level_command_is_public_and_the_manifest_subscribes_the_wand_hooks() {
    let manifest = manifest();
    assert_eq!(manifest.commands.len(), 2);
    for command in &manifest.commands {
        assert!(
            matches!(command, aether_plugin::manifest::CommandDecl::Plain(_)),
            "the top-level /rg must not be gated by one subcommand's node"
        );
    }
    assert!(manifest.subscribes("before-block-dig"));
    assert!(manifest.subscribes("before-block-interact"));
    assert!(manifest.subscribes("after-player-leave"));
    assert!(!manifest.capabilities.iter().any(|c| c == "world.read"));
}
