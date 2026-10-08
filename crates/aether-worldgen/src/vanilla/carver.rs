//! Carvers (stub).
use super::biome::BiomeRegistry;
use super::chunk::ProtoChunk;
use super::density::{BuildError, DataPack};
use super::generator::{Core, QuartCache};
use super::surface::SurfaceEnv;
use super::terrain::ChunkTerrain;

/// The configured carvers.
pub struct Carvers;

impl Carvers {
    /// Load.
    pub fn load(_pack: &DataPack, _biomes: &BiomeRegistry, _min_y: i32, _height: i32) -> Result<Self, BuildError> {
        Ok(Self)
    }

    pub(crate) fn carve(&self, _core: &Core, _c: &mut ProtoChunk, _t: &ChunkTerrain, _q: &QuartCache, _env: &impl SurfaceEnv) {}
}
