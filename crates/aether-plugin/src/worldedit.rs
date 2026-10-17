use std::sync::Arc;

use crate::service::ServiceRegistry;

pub const WORLDEDIT_SERVICE: &str = "worldedit.host";
pub const WORLDEDIT_SERVICE_VERSION: &str = "0.1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOutcome {
    pub changed: usize,
    pub undo_id: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerView {
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

pub trait WorldEditHost: Send + Sync {
    fn player_view(&self, caller: &str, player: u64) -> Option<PlayerView>;
    fn held_block(&self, caller: &str, player: u64) -> Option<String>;
    fn block_name(&self, caller: &str, x: i32, y: i32, z: i32) -> Option<String>;
    fn resolve_block(&self, caller: &str, name: &str) -> Option<String>;
    fn max_volume(&self, caller: &str) -> usize;
    fn check_build(
        &self,
        caller: &str,
        player: u64,
        min: [i32; 3],
        max: [i32; 3],
    ) -> Result<(), String>;
    fn begin_edit(&self, caller: &str, player: u64, expected_volume: usize) -> Result<u64, String>;
    fn set_block(
        &self,
        caller: &str,
        edit: u64,
        x: i32,
        y: i32,
        z: i32,
        block: &str,
    ) -> Result<(), String>;
    fn fill(
        &self,
        caller: &str,
        edit: u64,
        min: [i32; 3],
        max: [i32; 3],
        block: &str,
    ) -> Result<(), String>;
    fn commit(&self, caller: &str, edit: u64) -> Result<EditOutcome, String>;
    fn abort(&self, caller: &str, edit: u64);
    fn undo(&self, caller: &str, player: u64, undo_id: u64) -> Result<EditOutcome, String>;
    fn resolve_position(&self, caller: &str, player: u64, text: &str) -> Result<[i32; 3], String>;
}

pub struct WorldEditHostProvider(pub Arc<dyn WorldEditHost>);

pub fn register_host(
    registry: &ServiceRegistry,
    host: Arc<dyn WorldEditHost>,
) -> Result<(), String> {
    registry.register(
        WORLDEDIT_SERVICE,
        WORLDEDIT_SERVICE_VERSION,
        Arc::new(WorldEditHostProvider(host)),
    )
}

pub fn host(registry: &ServiceRegistry) -> Option<Arc<dyn WorldEditHost>> {
    registry
        .get::<WorldEditHostProvider>(WORLDEDIT_SERVICE, WORLDEDIT_SERVICE_VERSION)
        .map(|p| Arc::clone(&p.0))
}
