//! The biome registry: what each biome carries beyond its climate box.
//!
//! Read from the pack's `worldgen/biome/*.json`: base temperature and its
//! modifier (which decide snow), the carver list, and the eleven per-step
//! placed-feature lists that decoration walks.

use std::collections::HashMap;

use super::density::{BuildError, DataPack};
use super::json::Json;
use super::simplex::BiomeNoises;

/// An index into [`BiomeRegistry`].
pub type BiomeId = u16;

/// One biome's generation-relevant settings.
#[derive(Debug, Clone)]
pub struct BiomeInfo {
    /// Namespaced id, e.g. `minecraft:plains`.
    pub name: String,
    /// Base temperature.
    pub temperature: f32,
    /// Downfall.
    pub downfall: f32,
    /// Whether it rains/snows at all.
    pub has_precipitation: bool,
    /// `temperature_modifier: frozen`.
    pub frozen: bool,
    /// Configured carver ids, in order.
    pub carvers: Vec<String>,
    /// Placed-feature ids per decoration step.
    pub features: Vec<Vec<String>>,
}

/// Every biome in the pack, indexed by [`BiomeId`].
pub struct BiomeRegistry {
    biomes: Vec<BiomeInfo>,
    by_name: HashMap<String, BiomeId>,
    noises: BiomeNoises,
}

fn strings(j: Option<&Json>) -> Vec<String> {
    match j {
        Some(Json::Arr(a)) => a.iter().filter_map(Json::as_str).map(str::to_string).collect(),
        Some(Json::Str(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

impl BiomeRegistry {
    /// Read every `worldgen/biome/*.json`, sorted by id.
    pub fn load(pack: &DataPack) -> Result<Self, BuildError> {
        let mut biomes = Vec::new();
        let mut by_name = HashMap::new();
        for id in pack.list("biome")? {
            let j = pack.read_json("biome", &id)?;
            let features = match j.get("features") {
                Some(Json::Arr(steps)) => steps.iter().map(|s| strings(Some(s))).collect(),
                _ => Vec::new(),
            };
            let info = BiomeInfo {
                name: id.clone(),
                temperature: j.f64_or("temperature", 0.5) as f32,
                downfall: j.f64_or("downfall", 0.5) as f32,
                has_precipitation: j.bool_or("has_precipitation", true),
                frozen: j.str_of("temperature_modifier") == Some("frozen"),
                carvers: strings(j.get("carvers")),
                features,
            };
            by_name.insert(id, biomes.len() as BiomeId);
            biomes.push(info);
        }
        if biomes.is_empty() {
            return Err(BuildError::new("the pack has no biomes"));
        }
        Ok(Self {
            biomes,
            by_name,
            noises: BiomeNoises::new(),
        })
    }

    /// The id for a biome name.
    pub fn id(&self, name: &str) -> Option<BiomeId> {
        self.by_name.get(name).copied()
    }

    /// A biome's settings.
    pub fn get(&self, id: BiomeId) -> &BiomeInfo {
        &self.biomes[id as usize]
    }

    /// A biome's name.
    pub fn name(&self, id: BiomeId) -> &str {
        &self.biomes[id as usize].name
    }

    /// All biomes, in id order.
    pub fn all(&self) -> &[BiomeInfo] {
        &self.biomes
    }

    /// `Biome.getHeightAdjustedTemperature` (the game memoizes it; the value
    /// is a pure function of the position).
    pub fn temperature_at(&self, id: BiomeId, x: i32, y: i32, z: i32, sea_level: i32) -> f32 {
        let b = &self.biomes[id as usize];
        let base = if b.frozen {
            self.frozen_modifier(x, z, b.temperature)
        } else {
            b.temperature
        };
        let cut = sea_level + 17;
        if y > cut {
            let n = (self.noises.temperature.get_value(
                (x as f32 / 8.0) as f64,
                (z as f32 / 8.0) as f64,
                false,
            ) * 8.0) as f32;
            base - (n + y as f32 - cut as f32) * 0.05 / 40.0
        } else {
            base
        }
    }

    fn frozen_modifier(&self, x: i32, z: i32, temperature: f32) -> f32 {
        let a = self
            .noises
            .frozen_temperature
            .get_value(x as f64 * 0.05, z as f64 * 0.05, false)
            * 7.0;
        let b = self.noises.biome_info.get_value(x as f64 * 0.2, z as f64 * 0.2, false);
        if a + b < 0.3 {
            let c = self.noises.biome_info.get_value(x as f64 * 0.09, z as f64 * 0.09, false);
            if c < 0.8 {
                return 0.2;
            }
        }
        temperature
    }

    /// `Biome.coldEnoughToSnow`.
    pub fn cold_enough_to_snow(&self, id: BiomeId, x: i32, y: i32, z: i32, sea_level: i32) -> bool {
        self.temperature_at(id, x, y, z, sea_level) < 0.15
    }

    /// `Biome.shouldMeltFrozenOceanIcebergSlightly`.
    pub fn should_melt_iceberg_slightly(&self, id: BiomeId, x: i32, y: i32, z: i32, sea_level: i32) -> bool {
        self.temperature_at(id, x, y, z, sea_level) > 0.1
    }
}
