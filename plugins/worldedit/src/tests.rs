use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use aether_plugin::service::ServiceRegistry;
use aether_plugin::worldedit::{
    self, EditOutcome, PlayerView, WorldEditHost, WORLDEDIT_SERVICE, WORLDEDIT_SERVICE_VERSION,
};
use aether_plugin::{HostBackend, LogLevel};

type Pos = (i32, i32, i32);

struct FakeEdit {
    caller: String,
    blocks: HashMap<Pos, String>,
}

struct FakeHost {
    max: usize,
    view: Mutex<PlayerView>,
    held: Mutex<Option<String>>,
    blocks: Mutex<HashMap<Pos, String>>,
    region: Mutex<Option<([i32; 3], [i32; 3])>>,
    edits: Mutex<HashMap<u64, FakeEdit>>,
    next: AtomicU64,
    begins: AtomicUsize,
    commits: AtomicUsize,
    journal: Mutex<HashMap<u64, Vec<(Pos, String, String)>>>,
    schematics: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl FakeHost {
    fn new(max: usize) -> Self {
        FakeHost {
            max,
            view: Mutex::new(PlayerView {
                world: "minecraft:overworld".to_string(),
                x: 0.0,
                y: 64.0,
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
            }),
            held: Mutex::new(None),
            blocks: Mutex::new(HashMap::new()),
            region: Mutex::new(None),
            edits: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            begins: AtomicUsize::new(0),
            commits: AtomicUsize::new(0),
            journal: Mutex::new(HashMap::new()),
            schematics: Mutex::new(BTreeMap::new()),
        }
    }

    fn set_view(&self, yaw: f32) {
        self.view.lock().unwrap().yaw = yaw;
    }

    fn set_world(&self, world: &str) {
        self.view.lock().unwrap().world = world.to_string();
    }

    fn set_held(&self, item: Option<&str>) {
        *self.held.lock().unwrap() = item.map(str::to_string);
    }

    fn set_region(&self, min: [i32; 3], max: [i32; 3]) {
        *self.region.lock().unwrap() = Some((min, max));
    }

    fn block(&self, x: i32, y: i32, z: i32) -> String {
        self.blocks
            .lock()
            .unwrap()
            .get(&(x, y, z))
            .cloned()
            .unwrap_or_else(|| "minecraft:air".to_string())
    }

    fn put(&self, pos: Pos, block: &str) {
        self.blocks.lock().unwrap().insert(pos, block.to_string());
    }

    fn begins(&self) -> usize {
        self.begins.load(Ordering::SeqCst)
    }

    fn commits(&self) -> usize {
        self.commits.load(Ordering::SeqCst)
    }

    fn journal_len(&self) -> usize {
        self.journal.lock().unwrap().len()
    }

    fn canonical(name: &str) -> Option<String> {
        let base = name.split_once('[').map_or(name, |(head, _)| head);
        let bare = base.strip_prefix("minecraft:").unwrap_or(base);
        let known = [
            "stone",
            "dirt",
            "glass",
            "air",
            "water",
            "oak_stairs",
            "oak_log",
            "rail",
            "crafter",
            "oak_sign",
            "grass_block",
        ];
        known.contains(&bare).then(|| format!("minecraft:{bare}"))
    }
}

impl WorldEditHost for FakeHost {
    fn player_view(&self, _caller: &str, _player: u64) -> Option<PlayerView> {
        Some(self.view.lock().unwrap().clone())
    }

    fn held_block(&self, _caller: &str, _player: u64) -> Option<String> {
        let item = self.held.lock().unwrap().clone()?;
        FakeHost::canonical(&item)
    }

    fn block_name(&self, _caller: &str, x: i32, y: i32, z: i32) -> Option<String> {
        Some(self.block(x, y, z))
    }

    fn resolve_block(&self, _caller: &str, name: &str) -> Option<String> {
        FakeHost::canonical(name)
    }

    fn max_volume(&self, _caller: &str) -> usize {
        self.max
    }

    fn check_build(
        &self,
        _caller: &str,
        _player: u64,
        min: [i32; 3],
        max: [i32; 3],
    ) -> Result<(), String> {
        if let Some((rmin, rmax)) = *self.region.lock().unwrap() {
            let overlap = (0..3).all(|axis| min[axis] <= rmax[axis] && max[axis] >= rmin[axis]);
            if overlap {
                return Err("That area is protected.".to_string());
            }
        }
        Ok(())
    }

    fn check_cells(&self, _caller: &str, _player: u64, cells: &[[i32; 3]]) -> Result<(), String> {
        if let Some((rmin, rmax)) = *self.region.lock().unwrap() {
            for pos in cells {
                let inside = (0..3).all(|axis| pos[axis] >= rmin[axis] && pos[axis] <= rmax[axis]);
                if inside {
                    return Err("That area is protected.".to_string());
                }
            }
        }
        Ok(())
    }

    fn begin_edit(
        &self,
        caller: &str,
        _player: u64,
        expected_volume: usize,
    ) -> Result<u64, String> {
        if expected_volume > self.max {
            return Err(format!(
                "edit of {expected_volume} blocks exceeds the limit of {}",
                self.max
            ));
        }
        self.begins.fetch_add(1, Ordering::SeqCst);
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.edits.lock().unwrap().insert(
            id,
            FakeEdit {
                caller: caller.to_string(),
                blocks: HashMap::new(),
            },
        );
        Ok(id)
    }

    fn set_block(
        &self,
        caller: &str,
        edit: u64,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Result<(), String> {
        let mut edits = self.edits.lock().unwrap();
        let pending = edits
            .get_mut(&edit)
            .ok_or_else(|| "unknown edit".to_string())?;
        if pending.caller != caller {
            return Err("wrong caller".to_string());
        }
        FakeHost::canonical(block).ok_or_else(|| format!("unknown block `{block}`"))?;
        pending.blocks.insert((x, y, z), block.to_string());
        Ok(())
    }

    fn fill(
        &self,
        caller: &str,
        edit: u64,
        min: [i32; 3],
        max: [i32; 3],
        block: &str,
    ) -> Result<(), String> {
        let mut edits = self.edits.lock().unwrap();
        let pending = edits
            .get_mut(&edit)
            .ok_or_else(|| "unknown edit".to_string())?;
        if pending.caller != caller {
            return Err("wrong caller".to_string());
        }
        FakeHost::canonical(block).ok_or_else(|| format!("unknown block `{block}`"))?;
        for x in min[0]..=max[0] {
            for y in min[1]..=max[1] {
                for z in min[2]..=max[2] {
                    pending.blocks.insert((x, y, z), block.to_string());
                }
            }
        }
        Ok(())
    }

    fn commit(&self, caller: &str, edit: u64) -> Result<EditOutcome, String> {
        let pending = self
            .edits
            .lock()
            .unwrap()
            .remove(&edit)
            .ok_or_else(|| "unknown edit".to_string())?;
        if pending.caller != caller {
            return Err("wrong caller".to_string());
        }
        let mut changes = Vec::new();
        let mut blocks = self.blocks.lock().unwrap();
        for (pos, to) in pending.blocks {
            let from = blocks
                .get(&pos)
                .cloned()
                .unwrap_or_else(|| "minecraft:air".to_string());
            if from != to {
                changes.push((pos, from, to));
            }
        }
        changes.sort_by_key(|(pos, _, _)| *pos);
        if changes.is_empty() {
            return Ok(EditOutcome {
                changed: 0,
                undo_id: 0,
            });
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        for (pos, _, to) in &changes {
            blocks.insert(*pos, to.clone());
        }
        drop(blocks);
        self.journal.lock().unwrap().insert(id, changes.clone());
        self.commits.fetch_add(1, Ordering::SeqCst);
        Ok(EditOutcome {
            changed: changes.len(),
            undo_id: id,
        })
    }

    fn abort(&self, _caller: &str, edit: u64) {
        self.edits.lock().unwrap().remove(&edit);
    }

    fn undo(&self, _caller: &str, _player: u64, undo_id: u64) -> Result<EditOutcome, String> {
        let record = self
            .journal
            .lock()
            .unwrap()
            .remove(&undo_id)
            .ok_or_else(|| format!("no revision {undo_id}"))?;
        let mut inverse = Vec::new();
        let mut blocks = self.blocks.lock().unwrap();
        for (pos, from, to) in record {
            let now = blocks
                .get(&pos)
                .cloned()
                .unwrap_or_else(|| "minecraft:air".to_string());
            if now != from {
                inverse.push((pos, now, from.clone()));
                blocks.insert(pos, from);
            } else {
                let _ = to;
            }
        }
        if inverse.is_empty() {
            return Ok(EditOutcome {
                changed: 0,
                undo_id: 0,
            });
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.journal.lock().unwrap().insert(id, inverse.clone());
        Ok(EditOutcome {
            changed: inverse.len(),
            undo_id: id,
        })
    }

    fn resolve_position(
        &self,
        _caller: &str,
        _player: u64,
        text: &str,
    ) -> Result<[i32; 3], String> {
        let view = self.view.lock().unwrap();
        let base = [
            view.x.floor() as i32,
            view.y.floor() as i32,
            view.z.floor() as i32,
        ];
        let tokens: Vec<&str> = text
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .collect();
        if tokens.len() != 3 {
            return Err("expected three coordinates".to_string());
        }
        let mut out = [0; 3];
        for (i, token) in tokens.iter().enumerate() {
            out[i] = if let Some(rest) = token.strip_prefix('~') {
                if rest.is_empty() {
                    base[i]
                } else {
                    base[i] + rest.parse::<i32>().map_err(|_| "bad coordinate")?
                }
            } else {
                token.parse::<i32>().map_err(|_| "bad coordinate")?
            };
        }
        Ok(out)
    }

    fn save_schematic(&self, _caller: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.schematics
            .lock()
            .unwrap()
            .insert(name.to_string(), bytes.to_vec());
        Ok(())
    }

    fn load_schematic(&self, _caller: &str, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.schematics.lock().unwrap().get(name).cloned())
    }

    fn list_schematics(&self, _caller: &str) -> Vec<String> {
        self.schematics.lock().unwrap().keys().cloned().collect()
    }
}

#[derive(Default)]
struct TestBackend {
    held: Mutex<Option<String>>,
    given: Mutex<Vec<(u64, String, u32)>>,
    logs: Mutex<Vec<String>>,
}

impl HostBackend for TestBackend {
    fn kv_get(&self, _plugin: &str, _key: &str) -> Option<Vec<u8>> {
        None
    }
    fn kv_put(&self, _plugin: &str, _key: &str, _value: &[u8]) {}
    fn kv_delete(&self, _plugin: &str, _key: &str) {}
    fn kv_list(&self, _plugin: &str, _prefix: &str) -> Vec<String> {
        Vec::new()
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
    fn give_item(&self, _plugin: &str, player: u64, item: &str, count: u32) -> u64 {
        self.given
            .lock()
            .unwrap()
            .push((player, item.to_string(), count));
        0
    }
}

struct Fixture {
    plugin: WorldEditPlugin,
    host: Arc<FakeHost>,
    backend: Arc<TestBackend>,
    services: Arc<ServiceRegistry>,
    config: BTreeMap<String, String>,
}

impl Fixture {
    fn new(max: usize) -> Self {
        let host = Arc::new(FakeHost::new(max));
        let services = Arc::new(ServiceRegistry::new());
        let dynamic: Arc<dyn WorldEditHost> = host.clone();
        worldedit::register_host(&services, dynamic).unwrap();
        let backend = Arc::new(TestBackend::default());
        let config = BTreeMap::new();
        let mut plugin = WorldEditPlugin::default();
        let dynamic_backend: Arc<dyn HostBackend> = backend.clone();
        {
            let mut ctx = NativeContext {
                id: ID,
                capabilities: &[],
                config: &config,
                services: &services,
                backend: &dynamic_backend,
            };
            plugin.init(&mut ctx).unwrap();
        }
        Fixture {
            plugin,
            host,
            backend,
            services,
            config,
        }
    }

    fn command(&mut self, p: &Player, name: &str, args: &[&str]) -> Vec<String> {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        self.plugin
            .on_command(&mut ctx, p, name, &args)
            .expect("worldedit answers the command")
            .lines
    }

    fn set(&mut self, p: &Player, a: [i32; 3], b: [i32; 3]) {
        let (ax, ay, az) = (a[0].to_string(), a[1].to_string(), a[2].to_string());
        let (bx, by, bz) = (b[0].to_string(), b[1].to_string(), b[2].to_string());
        self.command(p, "pos1", &[&ax, &ay, &az]);
        self.command(p, "pos2", &[&bx, &by, &bz]);
    }

    fn dig(&mut self, p: &Player, pos: [i32; 3]) -> Decision {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        self.plugin
            .before_block_dig(&mut ctx, p, pos[0], pos[1], pos[2], "minecraft:stone")
    }

    fn interact(&mut self, p: &Player, pos: [i32; 3]) -> Decision {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &self.config,
            services: &self.services,
            backend: &backend,
        };
        self.plugin
            .before_block_interact(&mut ctx, p, pos[0], pos[1], pos[2], "minecraft:stone")
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

    fn reboot(&mut self) {
        let backend: Arc<dyn HostBackend> = self.backend.clone();
        let config = self.config.clone();
        let services = Arc::clone(&self.services);
        let mut plugin = WorldEditPlugin::default();
        let mut ctx = NativeContext {
            id: ID,
            capabilities: &[],
            config: &config,
            services: &services,
            backend: &backend,
        };
        plugin.init(&mut ctx).unwrap();
        self.plugin = plugin;
    }
}

fn player() -> Player {
    Player {
        id: 1,
        name: "Steve".to_string(),
        operator: true,
    }
}

fn error(lines: &[String]) -> bool {
    lines.iter().any(|line| line.starts_with("Error:"))
}

#[test]
fn a_legal_set_commits_once_and_changes_every_block() {
    let mut f = Fixture::new(1_000);
    let p = player();
    f.set(&p, [0, 0, 0], [1, 1, 1]);
    let lines = f.command(&p, "set", &["stone"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.commits(), 1, "one atomic commit");
    assert_eq!(f.host.journal_len(), 1, "one journal revision");
    for x in 0..=1 {
        for y in 0..=1 {
            for z in 0..=1 {
                assert_eq!(f.host.block(x, y, z), "minecraft:stone");
            }
        }
    }
}

#[test]
fn a_set_over_the_core_limit_is_refused_before_any_edit_starts() {
    let mut f = Fixture::new(10);
    let p = player();
    f.set(&p, [0, 0, 0], [1, 1, 5]);
    let lines = f.command(&p, "set", &["stone"]);
    assert!(error(&lines), "{lines:?}");
    assert_eq!(f.host.begins(), 0, "the edit never started");
    assert_eq!(f.host.commits(), 0);
    assert_eq!(f.host.block(0, 0, 0), "minecraft:air");
}

#[test]
fn weighted_patterns_and_hand_choose_blocks() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.set(&p, [0, 0, 0], [99, 0, 0]);
    let lines = f.command(&p, "set", &["50%stone,50:dirt"]);
    assert!(!error(&lines), "{lines:?}");
    let mut stone = 0;
    let mut dirt = 0;
    for x in 0..100 {
        match f.host.block(x, 0, 0).as_str() {
            "minecraft:stone" => stone += 1,
            "minecraft:dirt" => dirt += 1,
            other => panic!("unexpected block {other}"),
        }
    }
    assert!(stone > 0 && dirt > 0, "stone={stone} dirt={dirt}");

    f.host.set_held(Some("minecraft:glass"));
    f.set(&p, [0, 0, 0], [2, 0, 0]);
    let lines = f.command(&p, "set", &["#hand"]);
    assert!(!error(&lines), "{lines:?}");
    for x in 0..=2 {
        assert_eq!(f.host.block(x, 0, 0), "minecraft:glass");
    }
}

#[test]
fn existing_and_pos1_patterns_use_the_world_and_the_first_corner() {
    let mut f = Fixture::new(1_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    let lines = f.command(&p, "set", &["#existing"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(1, 0, 0), "minecraft:dirt");

    // `#pos1` takes the block at position 1 for every cell, including the
    // cell that currently holds it.
    f.set(&p, [10, 0, 10], [11, 0, 10]);
    f.host.put((10, 0, 10), "minecraft:glass");
    let lines = f.command(&p, "set", &["#pos1"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(10, 0, 10), "minecraft:glass");
    assert_eq!(f.host.block(11, 0, 10), "minecraft:glass");
}

#[test]
fn the_existing_filter_replaces_non_air_only() {
    let mut f = Fixture::new(1_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:air");
    f.host.put((2, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [2, 0, 0]);
    let lines = f.command(&p, "replace", &["#existing", "glass"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 0, 0), "minecraft:glass");
    assert_eq!(f.host.block(1, 0, 0), "minecraft:air");
    assert_eq!(f.host.block(2, 0, 0), "minecraft:glass");
}

#[test]
fn a_selection_from_another_world_is_refused() {
    let mut f = Fixture::new(1_000);
    let p = player();
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    f.host.set_world("the_nether");
    let lines = f.command(&p, "set", &["stone"]);
    assert!(error(&lines), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("belongs to")), "{lines:?}");
    assert_eq!(f.host.commits(), 0);
    assert_eq!(f.host.block(0, 0, 0), "minecraft:air");
}

#[test]
fn destructive_commands_require_op_two_and_selection_stays_public() {
    let manifest = manifest();
    let level = |name: &str| {
        manifest
            .command(name)
            .unwrap_or_else(|| panic!("{name} is declared"))
            .op()
    };
    for name in [
        "wand", "pos1", "pos2", "sel", "expand", "contract", "copy", "rotate", "flip", "mask",
    ] {
        assert_eq!(level(name), 0, "{name} does not write the world");
    }
    for name in [
        "set", "replace", "walls", "outline", "cut", "paste", "stack", "move", "schem", "brush",
        "undo", "redo",
    ] {
        assert_eq!(level(name), 2, "{name} writes the world, files or history");
    }
    assert!(!manifest.capabilities.iter().any(|c| c == "storage.kv"));
    assert!(!manifest.capabilities.iter().any(|c| c == "supports"));
}

#[test]
fn invalid_and_empty_patterns_error_without_a_commit() {
    let mut f = Fixture::new(1_000);
    let p = player();
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    let lines = f.command(&p, "set", &["badblock"]);
    assert!(error(&lines), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("unknown block")));
    let lines = f.command(&p, "set", &["#clipboard"]);
    assert!(error(&lines), "{lines:?}");
    assert_eq!(f.host.commits(), 0);
}

#[test]
fn replace_matches_a_filter_or_the_whole_selection() {
    let mut f = Fixture::new(1_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.host.put((2, 0, 0), "minecraft:glass");
    f.set(&p, [0, 0, 0], [2, 0, 0]);
    let lines = f.command(&p, "replace", &["stone", "dirt"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 0, 0), "minecraft:dirt");
    assert_eq!(f.host.block(1, 0, 0), "minecraft:dirt");
    assert_eq!(f.host.block(2, 0, 0), "minecraft:glass");

    let lines = f.command(&p, "replace", &["glass"]);
    assert!(!error(&lines), "{lines:?}");
    for x in 0..=2 {
        assert_eq!(f.host.block(x, 0, 0), "minecraft:glass");
    }
}

#[test]
fn walls_leave_the_inside_and_the_floor_centre_alone() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.set(&p, [0, 0, 0], [2, 2, 2]);
    let lines = f.command(&p, "walls", &["stone"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 1, 0), "minecraft:stone");
    assert_eq!(f.host.block(2, 2, 2), "minecraft:stone");
    assert_eq!(f.host.block(1, 1, 1), "minecraft:air");
    assert_eq!(f.host.block(1, 0, 1), "minecraft:air");
}

#[test]
fn outline_fills_the_shell_only() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.set(&p, [0, 0, 0], [2, 2, 2]);
    let lines = f.command(&p, "outline", &["dirt"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(1, 1, 1), "minecraft:air");
    assert_eq!(f.host.block(1, 1, 0), "minecraft:dirt");
    assert_eq!(f.host.block(0, 1, 1), "minecraft:dirt");
    assert_eq!(f.host.block(1, 2, 1), "minecraft:dirt");
}

#[test]
fn undo_redo_walks_multiple_edits_and_the_history_is_bounded() {
    let mut f = Fixture::new(1_000);
    let p = player();
    f.set(&p, [0, 0, 0], [0, 0, 0]);
    assert!(!error(&f.command(&p, "set", &["stone"])));
    assert!(!error(&f.command(&p, "set", &["dirt"])));
    assert!(!error(&f.command(&p, "set", &["glass"])));

    let lines = f.command(&p, "undo", &["2"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 0, 0), "minecraft:stone");
    let lines = f.command(&p, "redo", &["2"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 0, 0), "minecraft:glass");

    f.plugin.history_limit = 2;
    assert!(!error(&f.command(&p, "set", &["stone"])));
    assert!(!error(&f.command(&p, "set", &["dirt"])));
    assert!(!error(&f.command(&p, "set", &["glass"])));
    assert_eq!(f.plugin.history(p.id).undo.len(), 2, "bounded depth");
}

#[test]
fn expand_and_contract_follow_explicit_and_facing_directions() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.set(&p, [0, 0, 0], [1, 1, 1]);
    let lines = f.command(&p, "expand", &["2", "north"]);
    assert!(!error(&lines), "{lines:?}");
    assert!(
        lines[0].contains("0 - 0") || lines[0].contains("-2"),
        "{lines:?}"
    );
    let lines = f.command(&p, "expand", &["1", "up"]);
    assert!(!error(&lines), "{lines:?}");
    assert!(lines[0].contains("1 2 1"), "{lines:?}");
    let lines = f.command(&p, "contract", &["1", "north"]);
    assert!(!error(&lines), "{lines:?}");
    assert!(lines[0].contains("0 0 -1"), "{lines:?}");

    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.set_view(0.0);
    f.set(&p, [0, 0, 0], [1, 1, 1]);
    let lines = f.command(&p, "expand", &["3"]);
    assert!(!error(&lines), "{lines:?}");
    assert!(lines[0].contains("1 1 4"), "{lines:?}");
    f.host.set_view(90.0);
    let lines = f.command(&p, "expand", &["1"]);
    assert!(!error(&lines), "{lines:?}");
    assert!(lines[0].contains("-1 0 0"), "{lines:?}");
}

#[test]
fn a_selection_spanning_chunk_borders_edits_all_four_corners() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.set(&p, [-1, 0, -1], [17, 0, 17]);
    let lines = f.command(&p, "set", &["stone"]);
    assert!(!error(&lines), "{lines:?}");
    for pos in [
        (-1, 0, -1),
        (16, 0, 16),
        (-1, 0, 16),
        (16, 0, -1),
        (17, 0, 17),
    ] {
        assert_eq!(f.host.block(pos.0, pos.1, pos.2), "minecraft:stone");
    }
}

#[test]
fn a_protected_region_refuses_the_edit_without_a_bypass() {
    let mut f = Fixture::new(1_000);
    f.host.set_region([0, 0, 0], [3, 3, 3]);
    let p = player();
    f.set(&p, [1, 1, 1], [2, 2, 2]);
    let lines = f.command(&p, "set", &["stone"]);
    assert!(error(&lines), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("protected")), "{lines:?}");
    assert_eq!(f.host.begins(), 0);
    assert_eq!(f.host.block(1, 1, 1), "minecraft:air");
}

#[test]
fn selection_is_reset_when_the_player_leaves() {
    let mut f = Fixture::new(1_000);
    let p = player();
    f.set(&p, [0, 0, 0], [1, 1, 1]);
    assert!(!error(&f.command(&p, "sel", &[])));
    f.leave(&p);
    let lines = f.command(&p, "sel", &[]);
    assert!(error(&lines), "{lines:?}");
}

#[test]
fn wand_gives_the_item_and_both_clicks_set_positions() {
    let mut f = Fixture::new(1_000);
    let p = player();
    let lines = f.command(&p, "wand", &[]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(
        f.backend.given.lock().unwrap().as_slice(),
        &[(p.id, DEFAULT_WAND.to_string(), 1)]
    );

    *f.backend.held.lock().unwrap() = Some(DEFAULT_WAND.to_string());
    assert!(matches!(f.dig(&p, [1, 2, 3]), Decision::Cancel(_)));
    assert!(matches!(f.interact(&p, [5, 6, 7]), Decision::Cancel(_)));
    let lines = f.command(&p, "sel", &[]);
    assert!(lines[0].contains("1 2 3"), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("5 6 7")), "{lines:?}");
}

#[test]
fn pos_commands_accept_absolute_and_relative_coordinates() {
    let mut f = Fixture::new(1_000);
    let p = player();
    {
        let mut view = f.host.view.lock().unwrap();
        view.x = 9.0;
        view.y = 20.0;
        view.z = 30.0;
    }
    let lines = f.command(&p, "pos1", &["10", "20", "30"]);
    assert!(!error(&lines), "{lines:?}");
    let lines = f.command(&p, "pos2", &["~1", "~", "~-2"]);
    assert!(!error(&lines), "{lines:?}");
    let lines = f.command(&p, "sel", &[]);
    assert!(lines[0].contains("10 20 28"), "{lines:?}");
}

#[test]
fn the_registered_service_is_resolvable_by_name_and_version() {
    let host = Arc::new(FakeHost::new(1));
    let services = Arc::new(ServiceRegistry::new());
    let dynamic: Arc<dyn WorldEditHost> = host;
    worldedit::register_host(&services, dynamic).unwrap();
    assert!(services.contains(&aether_plugin::service::ServiceRef::new(
        WORLDEDIT_SERVICE,
        WORLDEDIT_SERVICE_VERSION
    )));
    assert!(worldedit::host(&services).is_some());
}

#[test]
fn copy_and_paste_at_the_same_place_and_after_the_player_moves() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.host.put((0, 1, 0), "minecraft:glass");
    f.set(&p, [0, 0, 0], [1, 1, 0]);
    let lines = f.command(&p, "copy", &[]);
    assert!(!error(&lines), "{lines:?}");

    for pos in [(0, 0, 0), (1, 0, 0), (0, 1, 0)] {
        f.host.put(pos, "minecraft:air");
    }
    let lines = f.command(&p, "paste", &["-o"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(1, 0, 0), "minecraft:dirt");
    assert_eq!(f.host.block(0, 1, 0), "minecraft:glass");

    f.host.view.lock().unwrap().x = 5.0;
    let lines = f.command(&p, "paste", &["-o"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(5, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(6, 0, 0), "minecraft:dirt");
    assert_eq!(f.host.block(5, 1, 0), "minecraft:glass");
}

#[test]
fn paste_defaults_to_air_and_a_skips_air_and_o_overwrites() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:air");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    assert!(!error(&f.command(&p, "copy", &[])));

    f.host.put((5, 0, 0), "minecraft:dirt");
    f.host.put((6, 0, 0), "minecraft:dirt");
    f.host.view.lock().unwrap().x = 5.0;
    assert!(!error(&f.command(&p, "paste", &[])));
    assert_eq!(f.host.block(5, 0, 0), "minecraft:dirt");
    assert_eq!(f.host.block(6, 0, 0), "minecraft:dirt");

    assert!(!error(&f.command(&p, "paste", &["-o"])));
    assert_eq!(f.host.block(5, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(6, 0, 0), "minecraft:air");

    f.host.put((5, 0, 0), "minecraft:dirt");
    f.host.put((6, 0, 0), "minecraft:glass");
    assert!(!error(&f.command(&p, "paste", &["-a", "-o", "-s"])));
    assert_eq!(f.host.block(5, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(6, 0, 0), "minecraft:glass");
    let lines = f.command(&p, "sel", &[]);
    assert!(
        lines[0].contains("5 0 0") && lines[0].contains("6 0 0"),
        "{lines:?}"
    );
}

#[test]
fn rotate_turns_the_clipboard_and_refuses_a_non_right_angle() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    assert!(!error(&f.command(&p, "copy", &[])));
    let original = f.plugin.clipboards.get(&p.id).cloned().unwrap();

    assert!(!error(&f.command(&p, "rotate", &["90"])));
    let rotated = f.plugin.clipboards.get(&p.id).cloned().unwrap();
    assert_eq!(rotated.size, [1, 1, 2]);
    assert_eq!(rotated.get(0, 0, 0), Some("minecraft:stone"));
    assert_eq!(rotated.get(0, 0, 1), Some("minecraft:dirt"));

    assert!(!error(&f.command(&p, "paste", &["-o"])));
    assert_eq!(f.host.block(0, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(0, 0, 1), "minecraft:dirt");

    let lines = f.command(&p, "rotate", &["45"]);
    assert!(error(&lines), "{lines:?}");
    assert_eq!(f.plugin.clipboards.get(&p.id), Some(&rotated));

    assert!(!error(&f.command(&p, "rotate", &["270"])));
    assert_eq!(f.plugin.clipboards.get(&p.id), Some(&original));
    assert!(!error(&f.command(&p, "rotate", &["90", "0", "0"])));
    assert_eq!(f.plugin.clipboards.get(&p.id).unwrap().size, [1, 1, 2]);
}

#[test]
fn flip_uses_an_axis_and_twice_returns_the_original() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((2, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [2, 0, 0]);
    assert!(!error(&f.command(&p, "copy", &[])));
    let original = f.plugin.clipboards.get(&p.id).cloned().unwrap();

    assert!(!error(&f.command(&p, "flip", &["east"])));
    let flipped = f.plugin.clipboards.get(&p.id).cloned().unwrap();
    assert_eq!(flipped.get(2, 0, 0), Some("minecraft:stone"));
    assert_eq!(flipped.get(0, 0, 0), Some("minecraft:dirt"));
    assert!(!error(&f.command(&p, "flip", &["west"])));
    assert_eq!(f.plugin.clipboards.get(&p.id), Some(&original));

    assert!(!error(&f.command(&p, "flip", &["up"])));
    assert!(!error(&f.command(&p, "flip", &["down"])));
    assert_eq!(f.plugin.clipboards.get(&p.id), Some(&original));
    let lines = f.command(&p, "flip", &["sideways"]);
    assert!(error(&lines), "{lines:?}");
}

#[test]
fn stack_copies_in_a_direction_in_one_revision() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    let lines = f.command(&p, "stack", &["2", "east"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.commits(), 1);
    assert_eq!(f.host.journal_len(), 1);
    for (x, block) in [
        (2, "minecraft:stone"),
        (3, "minecraft:dirt"),
        (4, "minecraft:stone"),
        (5, "minecraft:dirt"),
    ] {
        assert_eq!(f.host.block(x, 0, 0), block);
    }
    let lines = f.command(&p, "sel", &[]);
    assert!(
        lines[0].contains("0 0 0") && lines[0].contains("5 0 0"),
        "{lines:?}"
    );
}

#[test]
fn a_stack_over_the_limit_is_refused_before_it_starts() {
    let mut f = Fixture::new(3);
    let p = player();
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    let lines = f.command(&p, "stack", &["2", "east"]);
    assert!(error(&lines), "{lines:?}");
    assert_eq!(f.host.begins(), 0);
    assert_eq!(f.host.commits(), 0);
}

#[test]
fn move_accepts_both_argument_orders_and_clears_the_source() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    let lines = f.command(&p, "move", &["2", "east"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.commits(), 1);
    assert_eq!(f.host.block(0, 0, 0), "minecraft:air");
    assert_eq!(f.host.block(1, 0, 0), "minecraft:air");
    assert_eq!(f.host.block(2, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(3, 0, 0), "minecraft:dirt");

    f.set(&p, [10, 0, 0], [11, 0, 0]);
    f.host.put((10, 0, 0), "minecraft:stone");
    f.host.put((11, 0, 0), "minecraft:dirt");
    let lines = f.command(&p, "move", &["south", "1"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(10, 0, 0), "minecraft:air");
    assert_eq!(f.host.block(10, 0, 1), "minecraft:stone");
    assert_eq!(f.host.block(11, 0, 1), "minecraft:dirt");
}

#[test]
fn cut_fills_the_clipboard_and_clears_the_selection() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    let lines = f.command(&p, "cut", &[]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 0, 0), "minecraft:air");
    assert_eq!(f.host.block(1, 0, 0), "minecraft:air");
    assert_eq!(f.plugin.clipboards.get(&p.id).unwrap().volume(), 2);

    f.host.view.lock().unwrap().x = 5.0;
    assert!(!error(&f.command(&p, "paste", &["-o"])));
    assert_eq!(f.host.block(5, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(6, 0, 0), "minecraft:dirt");
}

#[test]
fn schematic_save_load_list_and_paste_survive_a_restart() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:oak_stairs[facing=north]");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    assert!(!error(&f.command(&p, "copy", &[])));
    let lines = f.command(&p, "schem", &["save", "house"]);
    assert!(!error(&lines), "{lines:?}");
    assert!(f.host.schematics.lock().unwrap().contains_key("house"));

    for pos in [(0, 0, 0), (1, 0, 0)] {
        f.host.put(pos, "minecraft:air");
    }
    let lines = f.command(&p, "schem", &["list"]);
    assert!(lines[0].contains("house"), "{lines:?}");

    f.reboot();
    let lines = f.command(&p, "schem", &["load", "house"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.plugin.clipboards.get(&p.id).unwrap().size, [2, 1, 1]);
    let lines = f.command(&p, "schem", &["paste", "house"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(0, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(1, 0, 0), "minecraft:oak_stairs[facing=north]");

    let lines = f.command(&p, "schem", &["load", "missing"]);
    assert!(error(&lines), "{lines:?}");
    let lines = f.command(&p, "schem", &["save", "../escape"]);
    assert!(error(&lines), "{lines:?}");
}

#[test]
fn a_brush_applies_through_the_bulk_path_and_undo_restores_it() {
    let mut f = Fixture::new(10_000);
    let p = player();
    *f.backend.held.lock().unwrap() = Some("minecraft:blaze_rod".to_string());
    let lines = f.command(&p, "brush", &["sphere", "stone", "1"]);
    assert!(!error(&lines), "{lines:?}");
    assert!(matches!(f.interact(&p, [10, 0, 10]), Decision::Cancel(_)));
    assert_eq!(f.host.block(10, 0, 10), "minecraft:stone");
    assert_eq!(f.host.block(11, 0, 10), "minecraft:stone");
    assert_eq!(f.host.block(10, 1, 10), "minecraft:stone");
    assert_eq!(f.host.block(11, 1, 10), "minecraft:air");
    assert_eq!(f.host.commits(), 1);
    let lines = f.command(&p, "undo", &[]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(10, 0, 10), "minecraft:air");
}

#[test]
fn a_brush_over_the_limit_or_in_a_region_is_refused() {
    let mut f = Fixture::new(5);
    let p = player();
    *f.backend.held.lock().unwrap() = Some("minecraft:blaze_rod".to_string());
    let lines = f.command(&p, "brush", &["sphere", "stone", "1"]);
    assert!(error(&lines), "{lines:?}");
    assert_eq!(f.host.begins(), 0);

    let mut f = Fixture::new(10_000);
    let p = player();
    *f.backend.held.lock().unwrap() = Some("minecraft:blaze_rod".to_string());
    assert!(!error(&f.command(&p, "brush", &["sphere", "stone", "1"])));
    f.host.set_region([10, 0, 10], [12, 2, 12]);
    assert!(matches!(f.interact(&p, [10, 0, 10]), Decision::Cancel(_)));
    assert_eq!(f.host.begins(), 0);
    assert_eq!(f.host.block(10, 0, 10), "minecraft:air");
}

#[test]
fn a_brush_mask_limits_the_cells_it_changes() {
    let mut f = Fixture::new(10_000);
    let p = player();
    *f.backend.held.lock().unwrap() = Some("minecraft:blaze_rod".to_string());
    f.host.put((10, 0, 10), "minecraft:stone");
    f.host.put((11, 0, 10), "minecraft:dirt");
    assert!(!error(&f.command(&p, "mask", &["stone"])));
    assert!(!error(&f.command(&p, "brush", &["smooth", "1"])));
    assert!(matches!(f.interact(&p, [10, 0, 10]), Decision::Cancel(_)));
    assert_eq!(f.host.block(10, 0, 10), "minecraft:air");
    assert_eq!(f.host.block(11, 0, 10), "minecraft:dirt");
}

#[test]
fn an_empty_clipboard_errors_for_every_clipboard_command() {
    let mut f = Fixture::new(10_000);
    let p = player();
    for (name, args) in [
        ("paste", &[][..]),
        ("rotate", &["90"][..]),
        ("flip", &[][..]),
        ("schem", &["save", "empty"][..]),
    ] {
        let lines = f.command(&p, name, args);
        assert!(error(&lines), "{name}: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("clipboard")), "{lines:?}");
    }
    assert_eq!(f.host.begins(), 0);
    assert_eq!(f.host.commits(), 0);
}

#[test]
fn a_clipboard_over_the_limit_is_refused_before_any_edit_starts() {
    let mut f = Fixture::new(5);
    let p = player();
    f.plugin.clipboards.insert(
        p.id,
        Clipboard {
            world: "minecraft:overworld".to_string(),
            origin: [0, 0, 0],
            offset: [0, 0, 0],
            size: [6, 1, 1],
            blocks: vec!["minecraft:stone".to_string(); 6],
        },
    );
    let lines = f.command(&p, "paste", &["-o"]);
    assert!(error(&lines), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("exceeds the limit")),
        "{lines:?}"
    );
    assert_eq!(f.host.begins(), 0);
    assert_eq!(f.host.commits(), 0);
}

#[test]
fn a_paste_in_a_protected_region_is_refused_and_undo_works_after_a_legal_one() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.set(&p, [0, 0, 0], [0, 0, 0]);
    assert!(!error(&f.command(&p, "copy", &[])));

    f.host.view.lock().unwrap().x = 10.0;
    f.host.set_region([10, 0, 0], [10, 2, 0]);
    let lines = f.command(&p, "paste", &["-o"]);
    assert!(error(&lines), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("protected")), "{lines:?}");
    assert_eq!(f.host.block(10, 0, 0), "minecraft:air");

    f.host.view.lock().unwrap().x = 30.0;
    assert!(!error(&f.command(&p, "paste", &["-o"])));
    assert_eq!(f.host.block(30, 0, 0), "minecraft:stone");
    let lines = f.command(&p, "undo", &[]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(30, 0, 0), "minecraft:air");
}

#[test]
fn the_clipboard_pattern_uses_the_copied_blocks() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    assert!(!error(&f.command(&p, "copy", &[])));
    f.set(&p, [10, 0, 0], [11, 0, 0]);
    let lines = f.command(&p, "set", &["#clipboard"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.block(10, 0, 0), "minecraft:stone");
    assert_eq!(f.host.block(11, 0, 0), "minecraft:dirt");
}

#[test]
fn rotate_pivots_about_the_copy_origin_when_the_player_is_not_at_a_corner() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((10, 0, 10), "minecraft:stone");
    f.host.put((11, 0, 10), "minecraft:glass");
    f.host.put((12, 0, 10), "minecraft:dirt");
    {
        let mut view = f.host.view.lock().unwrap();
        view.x = 11.0;
        view.z = 10.0;
    }
    f.set(&p, [10, 0, 10], [12, 0, 10]);
    assert!(!error(&f.command(&p, "copy", &[])));
    assert!(!error(&f.command(&p, "rotate", &["90"])));
    let lines = f.command(&p, "paste", &["-o"]);
    assert!(!error(&lines), "{lines:?}");
    // The bar rotates about the player (origin at x=11, z=10), not about its
    // minimum corner: it now runs north-south through x=11.
    assert_eq!(f.host.block(11, 0, 9), "minecraft:stone");
    assert_eq!(f.host.block(11, 0, 10), "minecraft:glass");
    assert_eq!(f.host.block(11, 0, 11), "minecraft:dirt");
    assert_eq!(f.host.block(10, 0, 9), "minecraft:air");
}

#[test]
fn a_rotated_paste_transforms_stairs_and_log_axes() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host
        .put((0, 0, 0), "minecraft:oak_stairs[facing=north,half=bottom]");
    f.host.put((1, 0, 0), "minecraft:oak_log[axis=x]");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    assert!(!error(&f.command(&p, "copy", &[])));
    assert!(!error(&f.command(&p, "rotate", &["90"])));
    f.host.view.lock().unwrap().x = 5.0;
    let lines = f.command(&p, "paste", &["-o"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(
        f.host.block(5, 0, 0),
        "minecraft:oak_stairs[facing=east,half=bottom]"
    );
    assert_eq!(f.host.block(5, 0, 1), "minecraft:oak_log[axis=z]");
}

#[test]
fn a_legal_brush_is_limited_by_its_exact_cell_count_not_its_bounding_cube() {
    // A radius-6 sphere covers 925 cells; its bounding cube is 2,197. With a
    // 1,000-block limit the brush is legal.
    let mut f = Fixture::new(1_000);
    let p = player();
    *f.backend.held.lock().unwrap() = Some("minecraft:blaze_rod".to_string());
    let lines = f.command(&p, "brush", &["sphere", "stone", "6"]);
    assert!(!error(&lines), "{lines:?}");
    assert!(matches!(f.interact(&p, [100, 0, 100]), Decision::Cancel(_)));
    assert_eq!(f.host.commits(), 1);
    assert_eq!(f.host.block(100, 0, 100), "minecraft:stone");
    assert_eq!(f.host.block(106, 0, 100), "minecraft:stone");
    assert_eq!(f.host.block(107, 0, 100), "minecraft:air");
}

#[test]
fn a_stack_that_exactly_fits_the_limit_is_accepted() {
    // Two source blocks stacked three times writes six cells; the source is
    // not part of the edit.
    let mut f = Fixture::new(6);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:stone");
    f.host.put((1, 0, 0), "minecraft:dirt");
    f.set(&p, [0, 0, 0], [1, 0, 0]);
    let lines = f.command(&p, "stack", &["3", "east"]);
    assert!(!error(&lines), "{lines:?}");
    assert_eq!(f.host.begins(), 1);
    assert_eq!(f.host.commits(), 1);
}

#[test]
fn flipping_and_pasting_two_way_rails_keeps_a_registered_state() {
    let mut f = Fixture::new(10_000);
    let p = player();
    f.host.put((0, 0, 0), "minecraft:rail[shape=east_west]");
    f.host.put((0, 0, 1), "minecraft:rail[shape=north_east]");
    f.host
        .put((0, 0, 2), "minecraft:crafter[orientation=down_east]");
    f.host.put((0, 0, 3), "minecraft:oak_sign[rotation=1]");
    f.set(&p, [0, 0, 0], [0, 0, 3]);
    assert!(!error(&f.command(&p, "copy", &[])));

    f.host.view.lock().unwrap().x = 10.0;
    let lines = f.command(&p, "flip", &["east"]);
    assert!(!error(&lines), "{lines:?}");
    let lines = f.command(&p, "paste", &["-o"]);
    assert!(
        !error(&lines),
        "the mirrored states must be valid: {lines:?}"
    );
    assert_eq!(f.host.block(10, 0, 0), "minecraft:rail[shape=east_west]");
    assert_eq!(f.host.block(10, 0, 1), "minecraft:rail[shape=north_west]");
    assert_eq!(
        f.host.block(10, 0, 2),
        "minecraft:crafter[orientation=down_west]"
    );
    assert_eq!(f.host.block(10, 0, 3), "minecraft:oak_sign[rotation=15]");
}
