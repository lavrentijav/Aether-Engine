use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aether_api::{JournalActor, World};
use aether_plugin::worldedit::{EditOutcome, PlayerView, WorldEditHost};
use aether_world::KvBackend;
use aether_worldgen::ChunkGenerator;

use crate::bulk::{self, BulkEdit, BulkHistory};
use crate::commands::coordinates::{Coordinates, WorldCoordinate};
use crate::players::SharedRegistry;
use crate::region::{Action, BlockPos, Verdict};

struct PendingEdit {
    caller: String,
    edit: BulkEdit,
}

pub struct Host<B: KvBackend + Send + Sync + 'static, G: ChunkGenerator + Send + Sync + 'static> {
    world: Arc<World<B, G>>,
    registry: SharedRegistry,
    edits: Mutex<HashMap<u64, PendingEdit>>,
    next_edit: AtomicU64,
    history: Mutex<BulkHistory>,
}

impl<B, G> Host<B, G>
where
    B: KvBackend + Send + Sync + 'static,
    G: ChunkGenerator + Send + Sync + 'static,
{
    pub fn new(world: Arc<World<B, G>>, registry: SharedRegistry) -> Self {
        Host {
            world,
            registry,
            edits: Mutex::new(HashMap::new()),
            next_edit: AtomicU64::new(1),
            history: Mutex::new(BulkHistory::new(64)),
        }
    }

    fn pending<T>(
        &self,
        caller: &str,
        edit: u64,
        f: impl FnOnce(&mut BulkEdit) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut edits = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        let pending = edits
            .get_mut(&edit)
            .ok_or_else(|| "unknown edit handle".to_string())?;
        if pending.caller != caller {
            return Err("edit handle belongs to another plugin".to_string());
        }
        f(&mut pending.edit)
    }

    fn block_id(&self, name: &str) -> Option<aether_api::BlockStateId> {
        if let Some(id) = self.world.block_id(name) {
            return Some(id);
        }
        let normalized = match name.split_once('[') {
            Some((base, rest)) if !base.contains(':') => {
                format!("minecraft:{base}[{rest}")
            }
            Some(_) => return None,
            None if name.contains(':') => return None,
            None => format!("minecraft:{name}"),
        };
        self.world.block_id(&normalized)
    }

    fn volume(min: [i32; 3], max: [i32; 3]) -> u128 {
        let dx = (max[0] as i64 - min[0] as i64 + 1) as u128;
        let dy = (max[1] as i64 - min[1] as i64 + 1) as u128;
        let dz = (max[2] as i64 - min[2] as i64 + 1) as u128;
        dx * dy * dz
    }
}

impl<B, G> WorldEditHost for Host<B, G>
where
    B: KvBackend + Send + Sync + 'static,
    G: ChunkGenerator + Send + Sync + 'static,
{
    fn player_view(&self, _caller: &str, player: u64) -> Option<PlayerView> {
        let handle = self.registry.by_entity(player as i32)?;
        let pos = handle.pos();
        Some(PlayerView {
            world: handle.world().to_string(),
            x: pos.x,
            y: pos.y,
            z: pos.z,
            yaw: pos.yaw,
            pitch: pos.pitch,
        })
    }

    fn held_block(&self, _caller: &str, player: u64) -> Option<String> {
        let handle = self.registry.by_entity(player as i32)?;
        let item = handle.held_item()?.vanilla_name()?.to_string();
        self.block_id(&item).map(|_| item)
    }

    fn block_name(&self, _caller: &str, x: i32, y: i32, z: i32) -> Option<String> {
        self.world.get_block_name(x, y, z)
    }

    fn resolve_block(&self, _caller: &str, name: &str) -> Option<String> {
        let id = self.block_id(name)?;
        self.world.block_name_of(id)
    }

    fn max_volume(&self, _caller: &str) -> usize {
        bulk::DEFAULT_MAX_BLOCKS
    }

    fn check_build(
        &self,
        _caller: &str,
        player: u64,
        min: [i32; 3],
        max: [i32; 3],
    ) -> Result<(), String> {
        if Self::volume(min, max) > bulk::DEFAULT_MAX_BLOCKS as u128 {
            return Err(format!(
                "selection exceeds the limit of {} blocks",
                bulk::DEFAULT_MAX_BLOCKS
            ));
        }
        let handle = self
            .registry
            .by_entity(player as i32)
            .ok_or_else(|| "player is not online".to_string())?;
        let store = crate::region::store();
        let index = store.snapshot();
        if index.is_empty() {
            return Ok(());
        }
        let world = handle.world();
        let actor = crate::region::actor_in(handle.as_ref(), world);
        for x in min[0]..=max[0] {
            for y in min[1]..=max[1] {
                for z in min[2]..=max[2] {
                    if index.check(world, BlockPos::new(x, y, z), Action::Place, &actor)
                        == Verdict::Deny
                    {
                        return Err(store.deny_message().to_string());
                    }
                }
            }
        }
        Ok(())
    }

    fn begin_edit(&self, caller: &str, player: u64, expected_volume: usize) -> Result<u64, String> {
        let max = bulk::DEFAULT_MAX_BLOCKS;
        if expected_volume > max {
            return Err(format!(
                "edit of {expected_volume} blocks exceeds the limit of {max}"
            ));
        }
        let handle = self
            .registry
            .by_entity(player as i32)
            .ok_or_else(|| "player is not online".to_string())?;
        let edit = BulkEdit::with_limit(JournalActor(handle.uuid), max);
        let id = self.next_edit.fetch_add(1, Ordering::Relaxed);
        self.edits.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id,
            PendingEdit {
                caller: caller.to_string(),
                edit,
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
        let id = self
            .block_id(block)
            .ok_or_else(|| format!("unknown block `{block}`"))?;
        self.pending(caller, edit, |pending| {
            pending
                .set(&*self.world, x, y, z, id)
                .map_err(|e| e.to_string())
        })
    }

    fn fill(
        &self,
        caller: &str,
        edit: u64,
        min: [i32; 3],
        max: [i32; 3],
        block: &str,
    ) -> Result<(), String> {
        let id = self
            .block_id(block)
            .ok_or_else(|| format!("unknown block `{block}`"))?;
        self.pending(caller, edit, |pending| {
            pending
                .fill(
                    &*self.world,
                    (min[0], min[1], min[2]),
                    (max[0], max[1], max[2]),
                    id,
                )
                .map_err(|e| e.to_string())
        })
    }

    fn commit(&self, caller: &str, edit: u64) -> Result<EditOutcome, String> {
        let pending = self
            .edits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&edit)
            .ok_or_else(|| "unknown edit handle".to_string())?;
        if pending.caller != caller {
            self.edits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(edit, pending);
            return Err("edit handle belongs to another plugin".to_string());
        }
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let commit = history
            .commit_with_changes(&*self.world, pending.edit)
            .map_err(|e| e.to_string())?;
        if !commit.changes.is_empty() {
            bulk::enqueue_publish(commit.actor, commit.changes);
        }
        Ok(EditOutcome {
            changed: commit.result.changed,
            undo_id: commit.result.undo_id,
        })
    }

    fn abort(&self, caller: &str, edit: u64) {
        let mut edits = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        if edits.get(&edit).is_some_and(|p| p.caller == caller) {
            edits.remove(&edit);
        }
    }

    fn undo(&self, _caller: &str, _player: u64, undo_id: u64) -> Result<EditOutcome, String> {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let result = history
            .undo(&*self.world, undo_id)
            .map_err(|e| e.to_string())?;
        Ok(EditOutcome {
            changed: result.changed,
            undo_id: result.undo_id,
        })
    }

    fn resolve_position(&self, _caller: &str, player: u64, text: &str) -> Result<[i32; 3], String> {
        let handle = self
            .registry
            .by_entity(player as i32)
            .ok_or_else(|| "player is not online".to_string())?;
        let tokens: Vec<&str> = text
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .collect();
        if tokens.len() != 3 {
            return Err("expected three coordinates".to_string());
        }
        let parsed = tokens
            .iter()
            .map(|t| WorldCoordinate::parse_block(t).map_err(|e| e.message()))
            .collect::<Result<Vec<_>, _>>()?;
        let coords = Coordinates::new(parsed[0], parsed[1], parsed[2]);
        let pos = coords
            .resolve_block(handle.as_ref())
            .map_err(|e| e.message())?;
        Ok([pos.x, pos.y, pos.z])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::{block_ids, MemStore};
    use aether_worldgen::FlatGenerator;

    use crate::players::{PlayerHandle, PosLook, Registry};

    type TestWorld = World<MemStore, FlatGenerator>;

    fn world() -> Arc<TestWorld> {
        Arc::new(World::new(MemStore::new(), FlatGenerator::classic()))
    }

    fn registry(players: &[(i32, &str, f64, f64, f64, f32)]) -> SharedRegistry {
        let registry = Arc::new(Registry::default());
        for &(id, name, x, y, z, yaw) in players {
            let handle = PlayerHandle::for_test(id, name);
            handle.set_pos(PosLook {
                x,
                y,
                z,
                yaw,
                pitch: 0.0,
                on_ground: true,
            });
            registry.insert_for_test(handle);
        }
        registry
    }

    fn pos(x: i32, y: i32, z: i32) -> [i32; 3] {
        [x, y, z]
    }

    #[test]
    fn a_legal_fill_commits_one_revision_and_undo_restores_it() {
        let world = world();
        let registry = registry(&[(1, "Steve", 0.0, 64.0, 0.0, 0.0)]);
        let host = Host::new(Arc::clone(&world), registry);
        let head = world.journal().head();

        let edit = host.begin_edit("worldedit", 1, 8).unwrap();
        host.fill(
            "worldedit",
            edit,
            pos(0, 20, 0),
            pos(1, 21, 1),
            "minecraft:stone",
        )
        .unwrap();
        let outcome = host.commit("worldedit", edit).unwrap();
        assert_eq!(outcome.changed, 8);
        assert_ne!(outcome.undo_id, 0);
        assert_eq!(world.journal().head(), head + 1, "one journal revision");
        assert_eq!(world.get_block(1, 21, 1), block_ids::STONE);

        let undo = host.undo("worldedit", 1, outcome.undo_id).unwrap();
        assert_eq!(undo.changed, 8);
        assert_eq!(world.get_block(1, 21, 1), block_ids::AIR);
    }

    #[test]
    fn an_edit_over_the_core_limit_is_refused_before_it_starts() {
        let world = world();
        let registry = registry(&[(1, "Steve", 0.0, 64.0, 0.0, 0.0)]);
        let host = Host::new(world, registry);
        let err = host
            .begin_edit("worldedit", 1, bulk::DEFAULT_MAX_BLOCKS + 1)
            .unwrap_err();
        assert!(err.contains("exceeds the limit"), "{err}");
        assert!(host.edits.lock().unwrap().is_empty());
    }

    #[test]
    fn a_protected_region_denies_a_fill_but_a_bypassing_operator_may_edit() {
        let _guard = crate::permissions::test_lock();
        crate::permissions::seed_operators(&["WorldEditOp".to_string()]);
        let _store = crate::region::install_test_store(crate::region::RegionStore::new());
        crate::region::store().set_deny_message("That area is protected.");
        crate::region::store().upsert(crate::region::Region::new(
            crate::region::RegionId(1),
            crate::region::OVERWORLD,
            crate::region::BlockPos::new(0, 0, 0),
            crate::region::BlockPos::new(3, 100, 3),
        ));

        let world = world();
        let registry = registry(&[
            (1, "WorldEditGuest", 0.0, 64.0, 0.0, 0.0),
            (2, "WorldEditOp", 0.0, 64.0, 0.0, 0.0),
        ]);
        let host = Host::new(world, registry);
        let err = host
            .check_build("worldedit", 1, pos(1, 20, 1), pos(2, 22, 2))
            .unwrap_err();
        assert!(err.contains("protected"), "{err}");
        assert!(host
            .check_build("worldedit", 2, pos(1, 20, 1), pos(2, 22, 2))
            .is_ok());
    }

    #[test]
    fn positions_resolve_through_the_core_coordinate_parser() {
        let world = world();
        let registry = registry(&[(1, "Steve", 10.5, 64.0, 30.5, 0.0)]);
        let host = Host::new(world, registry);
        assert_eq!(
            host.resolve_position("worldedit", 1, "~1 ~ ~-2").unwrap(),
            [11, 64, 28]
        );
        assert_eq!(
            host.resolve_position("worldedit", 1, "10,20,30").unwrap(),
            [10, 20, 30]
        );
        assert!(host.resolve_position("worldedit", 1, "1 2").is_err());
    }

    #[test]
    fn blocks_resolve_and_the_hand_only_answers_with_a_block() {
        let world = world();
        let registry = registry(&[(1, "Steve", 0.0, 64.0, 0.0, 0.0)]);
        let handle = registry.by_entity(1).unwrap();
        handle.seed_hotbar(&[("minecraft:wooden_axe".to_string(), 1)]);
        let host = Host::new(Arc::clone(&world), Arc::clone(&registry));
        assert_eq!(
            host.resolve_block("worldedit", "stone").as_deref(),
            Some("minecraft:stone")
        );
        assert!(host.resolve_block("worldedit", "not_a_block").is_none());
        assert!(host.held_block("worldedit", 1).is_none());
        handle.seed_hotbar(&[("minecraft:stone".to_string(), 64)]);
        assert_eq!(
            host.held_block("worldedit", 1).as_deref(),
            Some("minecraft:stone")
        );
    }

    #[test]
    fn an_unknown_edit_handle_is_refused() {
        let world = world();
        let registry = registry(&[(1, "Steve", 0.0, 64.0, 0.0, 0.0)]);
        let host = Host::new(world, registry);
        assert!(host
            .set_block("worldedit", 99, 0, 20, 0, "minecraft:stone")
            .is_err());
        assert!(host.commit("worldedit", 99).is_err());
        assert!(host
            .begin_edit("worldedit", 5, 1)
            .unwrap_err()
            .contains("not online"));
    }
}
