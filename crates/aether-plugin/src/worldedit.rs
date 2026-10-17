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
    /// The region check over exactly the cells an operation will touch.
    ///
    /// A paste with `-a`, a move and a brush change a subset of their bounding
    /// box, so checking the box would refuse a legal edit that merely shares a
    /// corner with a protected region. The core still owns the verdict and the
    /// bypass nodes; the plugin only names the cells.
    fn check_cells(&self, caller: &str, player: u64, cells: &[[i32; 3]]) -> Result<(), String>;
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

    /// Persist a schematic's bytes under `name`. The host owns the directory
    /// and the file extension; the plugin owns the `.schem` encoding.
    fn save_schematic(&self, caller: &str, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// Read a schematic's bytes, or `None` when no schematic has that name.
    fn load_schematic(&self, caller: &str, name: &str) -> Result<Option<Vec<u8>>, String>;
    /// The schematic names visible to the caller, sorted.
    fn list_schematics(&self, caller: &str) -> Vec<String>;
}

/// A schematic name is a single file stem: ASCII letters, digits, `_` and `-`.
/// The host applies the same rule, so neither a plugin nor a file on disk can
/// escape the schematic directory.
pub fn valid_schematic_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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
