//! Features (stub).
use std::sync::Arc;

use aether_world::BlockStateId;

use super::biome::{BiomeId, BiomeRegistry};
use super::chunk::ProtoChunk;
use super::density::{BuildError, DataPack};
use super::generator::Core;
use super::overworld::OverworldBiomeSource;

/// The decorator.
pub struct Decorator;

impl Decorator {
    /// Load.
    pub fn load(_pack: &DataPack, _biomes: &BiomeRegistry, _src: &OverworldBiomeSource, _ids: &[BiomeId], _min_y: i32, _height: i32) -> Result<Self, BuildError> {
        Ok(Self)
    }

    /// Unsupported.
    pub fn unsupported(&self) -> Vec<String> {
        Vec::new()
    }

    pub(crate) fn decorate(&self, _core: &Core, _cx: i32, _cz: i32, _region: &[[Arc<ProtoChunk>; 3]; 3]) -> Vec<(i32, i32, i32, BlockStateId)> {
        Vec::new()
    }
}
