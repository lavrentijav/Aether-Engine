//! Vanilla structures: where they start and, for template structures, what
//! they place.
//!
//! `structure_set` files in the pack pair a [`placement::StructurePlacement`]
//! with one or more structures and weights. This module reads them and decides
//! which chunks start a structure ([`StructureRegistry::starts_at`]) exactly as
//! the game's `ChunkGeneratorStructureState` / `ChunkGenerator.createStructures`
//! do. [`template`] is the one structure-template reader and placer, shared by
//! the igloo ([`igloo`]) and the fossil feature.
//!
//! # Groundwork boundaries
//!
//! * Placement is exact — see [`placement`].
//! * The weighted choice among a set's structures returns the *first* draw;
//!   vanilla would fall through to the next entry when the biome at the
//!   generation position does not match, which needs the terrain. That check
//!   belongs to generation and is not modelled yet.
//! * Of the sets' structures, only the igloo is placed (from the pack's `.nbt`
//!   templates); jigsaw structures (villages, …) are not — see
//!   `docs/tasks/QF-notes.md`.

pub mod igloo;
pub mod placement;
pub mod template;

use std::collections::{HashMap, HashSet};

use crate::vanilla::biome::{BiomeId, BiomeRegistry};
use crate::vanilla::density::{BuildError, DataPack};
use crate::vanilla::json::Json;
use crate::vanilla::overworld::OverworldBiomeSource;
use crate::vanilla::rng::{Rng, WorldgenRandom};

use placement::{ChunkPos, StructurePlacement};

/// One structure set: a placement plus its weighted structures.
#[derive(Debug, Clone)]
pub struct StructureSetInfo {
    /// The registry id, e.g. `minecraft:igloos`.
    pub id: String,
    /// Where it starts.
    pub placement: StructurePlacement,
    /// `(structure id, weight)` in file order.
    pub structures: Vec<(String, i32)>,
    /// Stronghold chunk positions, precomputed; empty for `random_spread`.
    pub ring_positions: Vec<ChunkPos>,
}

/// A structure that starts in a chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    /// Index into [`StructureRegistry::sets`].
    pub set: usize,
    /// The structure id selected for this chunk.
    pub structure: String,
}

impl StructureSetInfo {
    /// The set's `biomes` gate is handled at load; this is the pure placement.
    fn is_placement_start(&self, seed: i64, cx: i32, cz: i32) -> bool {
        match &self.placement {
            StructurePlacement::RandomSpread(p) => p.is_start_chunk(seed, cx, cz),
            StructurePlacement::ConcentricRings(p) => {
                p.is_start_chunk_in(&self.ring_positions, cx, cz) && p.common.allows(seed, cx, cz)
            }
        }
    }

    /// The first weighted pick among the set's structures.
    fn pick_structure(&self, seed: i64, cx: i32, cz: i32) -> Option<String> {
        if self.structures.is_empty() {
            return None;
        }
        if self.structures.len() == 1 {
            return Some(self.structures[0].0.clone());
        }
        let mut r = WorldgenRandom::legacy(0);
        r.set_large_feature_seed(seed, cx, cz);
        let total: i32 = self.structures.iter().map(|e| e.1).sum();
        if total <= 0 {
            return None;
        }
        let mut j = r.next_int_bounded(total);
        for (id, w) in &self.structures {
            j -= w;
            if j < 0 {
                return Some(id.clone());
            }
        }
        Some(self.structures.last().unwrap().0.clone())
    }
}

/// Every structure set the overworld's biomes can use, with precomputed
/// stronghold rings.
pub struct StructureRegistry {
    sets: Vec<StructureSetInfo>,
    by_id: HashMap<String, usize>,
}

impl StructureRegistry {
    /// Build directly from sets (tests, or a caller that loaded them itself).
    pub fn from_sets(sets: Vec<StructureSetInfo>) -> Self {
        let by_id = sets
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i))
            .collect();
        Self { sets, by_id }
    }

    /// The sets, in load order.
    pub fn sets(&self) -> &[StructureSetInfo] {
        &self.sets
    }

    /// The empty registry.
    pub fn empty() -> Self {
        Self {
            sets: Vec::new(),
            by_id: HashMap::new(),
        }
    }

    /// Every structure that starts in `(cx, cz)`, in set order. A chunk can
    /// start one structure per set.
    pub fn starts_at(&self, seed: i64, cx: i32, cz: i32) -> Vec<Start> {
        let mut out = Vec::new();
        let mut stack = Vec::new();
        for i in 0..self.sets.len() {
            if self.is_structure_chunk(i, seed, cx, cz, &mut stack) {
                if let Some(structure) = self.sets[i].pick_structure(seed, cx, cz) {
                    out.push(Start { set: i, structure });
                }
            }
        }
        out
    }

    /// `StructurePlacement.isStructureChunk`: placement, frequency gate, then
    /// exclusion-zone interaction with other sets.
    fn is_structure_chunk(
        &self,
        i: usize,
        seed: i64,
        cx: i32,
        cz: i32,
        stack: &mut Vec<usize>,
    ) -> bool {
        let set = &self.sets[i];
        if !set.is_placement_start(seed, cx, cz) {
            return false;
        }
        let Some(zone) = set.placement.common().exclusion_zone.as_ref() else {
            return true;
        };
        // A pack with mutually-excluding sets would recurse forever in vanilla;
        // stop on a cycle rather than blow the stack.
        if stack.contains(&i) {
            return true;
        }
        stack.push(i);
        let forbidden =
            self.other_set_in_range(&zone.other_set, seed, cx, cz, zone.chunk_count, stack);
        stack.pop();
        !forbidden
    }

    /// `hasStructureChunkInRange(otherSet, cx, cz, k)`.
    fn other_set_in_range(
        &self,
        other: &str,
        seed: i64,
        cx: i32,
        cz: i32,
        k: i32,
        stack: &mut Vec<usize>,
    ) -> bool {
        let Some(&j) = self.by_id.get(other) else {
            return false;
        };
        for l in cx - k..=cx + k {
            for m in cz - k..=cz + k {
                if self.is_structure_chunk(j, seed, l, m, stack) {
                    return true;
                }
            }
        }
        false
    }

    /// Read every `worldgen/structure_set/*.json` the overworld can use.
    pub fn load(
        pack: &DataPack,
        biome_source: &OverworldBiomeSource,
        biomes: &BiomeRegistry,
        entry_ids: &[BiomeId],
        seed: i64,
    ) -> Result<Self, BuildError> {
        // `possibleBiomes`: the distinct biomes the source can produce.
        let mut possible: HashSet<String> = HashSet::new();
        for e in biome_source.biomes().entries() {
            possible.insert(e.biome.clone());
        }
        // Row -> biome id, for the stronghold search.
        let row_id = |row: usize| entry_ids[row];

        let mut sets = Vec::new();
        for id in pack.list("structure_set")? {
            let j = pack.read_json("structure_set", &id)?;
            let placement_json = match j.get("placement") {
                Some(p) => p,
                None => continue,
            };
            let holder_set = |v: Option<&Json>| biome_holder_set(pack, biomes, v);
            let placement = StructurePlacement::parse_with(placement_json, &holder_set)
                .map_err(|e| BuildError::new(format!("{id}: {e}")))?;
            let structures = parse_structures(&j);
            if structures.is_empty() {
                continue;
            }
            // `hasBiomesForStructureSet`: keep the set only if one of its
            // structures can appear in one of the source's biomes.
            let usable = structures.iter().any(|(sid, _)| {
                let names = structure_biome_names(pack, sid);
                names.iter().any(|n| possible.contains(n))
            });
            if !usable {
                continue;
            }
            let ring_positions = match &placement {
                StructurePlacement::ConcentricRings(p) => {
                    let preferred: HashSet<u16> = p.preferred.iter().copied().collect();
                    let biome_at =
                        |qx: i32, qy: i32, qz: i32| row_id(biome_source.entry_at(qx, qy, qz));
                    let pref = move |b: u16| preferred.contains(&b);
                    p.ring_positions(seed, &biome_at, &pref)
                }
                _ => Vec::new(),
            };
            sets.push(StructureSetInfo {
                id,
                placement,
                structures,
                ring_positions,
            });
        }
        Ok(Self::from_sets(sets))
    }
}

fn parse_structures(j: &Json) -> Vec<(String, i32)> {
    j.get("structures")
        .and_then(Json::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter_map(|e| {
            let id = e.str_of("structure")?.to_string();
            let w = e.i32_or("weight", 1);
            Some((id, w))
        })
        .collect()
}

/// The biome names a structure can appear in, from its `biomes` holder set
/// (a tag, a single biome, or a list).
fn structure_biome_names(pack: &DataPack, structure_id: &str) -> Vec<String> {
    let Ok(j) = pack.read_json("structure", structure_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_biome_names(pack, j.get("biomes"), &mut out, 0);
    out
}

/// The biome ids a structure can appear in — `Structure.biomes()`, resolved.
pub fn structure_biomes(
    pack: &DataPack,
    biomes: &BiomeRegistry,
    structure_id: &str,
) -> Vec<BiomeId> {
    let mut ids: Vec<BiomeId> = structure_biome_names(pack, structure_id)
        .iter()
        .filter_map(|n| biomes.id(n))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// A `HolderSet<Biome>` from JSON: `"#tag"`, `"minecraft:biome"` or a list.
fn biome_holder_set(pack: &DataPack, biomes: &BiomeRegistry, v: Option<&Json>) -> Vec<u16> {
    let mut names = Vec::new();
    collect_biome_names(pack, v, &mut names, 0);
    let mut ids: Vec<u16> = names.iter().filter_map(|n| biomes.id(n)).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn collect_biome_names(pack: &DataPack, v: Option<&Json>, out: &mut Vec<String>, depth: usize) {
    if depth > 32 {
        return;
    }
    match v {
        Some(Json::Str(s)) if s.starts_with('#') => {
            let tag = s.trim_start_matches('#');
            let Some(j) = pack.tag_json("worldgen/biome", tag) else {
                return;
            };
            for entry in j.get("values").and_then(Json::as_arr).unwrap_or(&[]) {
                let name = match entry {
                    Json::Str(s) => s.as_str(),
                    Json::Obj(_) => entry.str_of("id").unwrap_or(""),
                    _ => continue,
                };
                if name.is_empty() {
                    continue;
                }
                if let Some(t) = name.strip_prefix('#') {
                    let tag = Json::Str(format!("#{t}"));
                    collect_biome_names(pack, Some(&tag), out, depth + 1);
                } else {
                    out.push(name.to_string());
                }
            }
        }
        Some(Json::Str(s)) => out.push(s.clone()),
        Some(Json::Arr(a)) => {
            for e in a {
                collect_biome_names(pack, Some(e), out, depth + 1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::placement::{Common, ConcentricRings, FrequencyReduction, RandomSpread, SpreadType};
    use super::*;
    use crate::vanilla::json::Json;

    fn igloo_set() -> StructureSetInfo {
        StructureSetInfo {
            id: "minecraft:igloos".into(),
            placement: StructurePlacement::RandomSpread(RandomSpread {
                common: Common {
                    locate_offset: (0, 0, 0),
                    frequency: 1.0,
                    reduction: FrequencyReduction::Default,
                    salt: 14_357_618,
                    exclusion_zone: None,
                },
                spacing: 32,
                separation: 8,
                spread_type: SpreadType::Linear,
            }),
            structures: vec![("minecraft:igloo".into(), 1)],
            ring_positions: Vec::new(),
        }
    }

    #[test]
    fn a_start_chunk_reports_its_structure() {
        let reg = StructureRegistry::from_sets(vec![igloo_set()]);
        // From the golden: seed 42 region (0,0) starts at chunk (1,6).
        assert_eq!(
            reg.starts_at(42, 1, 6),
            vec![Start {
                set: 0,
                structure: "minecraft:igloo".into()
            }]
        );
        assert!(reg.starts_at(42, 0, 0).is_empty());
        assert!(reg.starts_at(42, 1, 7).is_empty());
    }

    #[test]
    fn a_chunk_starts_at_most_one_per_set() {
        let reg = StructureRegistry::from_sets(vec![igloo_set()]);
        let mut starts = 0;
        for cx in 0..32 {
            for cz in 0..32 {
                starts += reg.starts_at(42, cx, cz).len();
            }
        }
        assert_eq!(starts, 1);
    }

    #[test]
    fn weighted_selection_consumes_the_seeded_draw() {
        // Villages: five equal weights. The pick for a known chunk follows
        // `setLargeFeatureSeed(seed, cx, cz)` then one bounded draw.
        let set = StructureSetInfo {
            id: "minecraft:villages".into(),
            placement: StructurePlacement::RandomSpread(RandomSpread {
                common: Common {
                    locate_offset: (0, 0, 0),
                    frequency: 1.0,
                    reduction: FrequencyReduction::Default,
                    salt: 10_387_312,
                    exclusion_zone: None,
                },
                spacing: 34,
                separation: 8,
                spread_type: SpreadType::Linear,
            }),
            structures: vec![
                ("minecraft:village_plains".into(), 1),
                ("minecraft:village_desert".into(), 1),
                ("minecraft:village_savanna".into(), 1),
                ("minecraft:village_snowy".into(), 1),
                ("minecraft:village_taiga".into(), 1),
            ],
            ring_positions: Vec::new(),
        };
        let reg = StructureRegistry::from_sets(vec![set]);
        // Find the region's start chunk, then confirm the pick is stable.
        let mut found = None;
        for cx in 0..34 {
            for cz in 0..34 {
                if let Some(s) = reg.starts_at(42, cx, cz).first() {
                    found = Some((cx, cz, s.structure.clone()));
                }
            }
        }
        let (cx, cz, first) = found.expect("a village start exists");
        let again = reg.starts_at(42, cx, cz).remove(0).structure;
        assert_eq!(first, again);
        assert!(first.starts_with("minecraft:village_"));
    }

    #[test]
    fn concentric_ring_sets_use_precomputed_positions() {
        let set = StructureSetInfo {
            id: "minecraft:strongholds".into(),
            placement: StructurePlacement::ConcentricRings(ConcentricRings {
                common: Common {
                    locate_offset: (0, 0, 0),
                    frequency: 1.0,
                    reduction: FrequencyReduction::Default,
                    salt: 0,
                    exclusion_zone: None,
                },
                distance: 32,
                spread: 3,
                count: 4,
                preferred: vec![1],
            }),
            structures: vec![("minecraft:stronghold".into(), 1)],
            ring_positions: vec![(10, 20), (-30, 40)],
        };
        let reg = StructureRegistry::from_sets(vec![set]);
        assert_eq!(reg.starts_at(0, 10, 20).len(), 1);
        assert_eq!(reg.starts_at(0, -30, 40).len(), 1);
        assert!(reg.starts_at(0, 0, 0).is_empty());
    }

    #[test]
    fn exclusion_zone_forbids_near_another_set() {
        let villages = StructureSetInfo {
            id: "minecraft:villages".into(),
            placement: StructurePlacement::RandomSpread(RandomSpread {
                common: Common {
                    locate_offset: (0, 0, 0),
                    frequency: 1.0,
                    reduction: FrequencyReduction::Default,
                    salt: 10_387_312,
                    exclusion_zone: None,
                },
                spacing: 34,
                separation: 8,
                spread_type: SpreadType::Linear,
            }),
            structures: vec![("minecraft:village_plains".into(), 1)],
            ring_positions: Vec::new(),
        };
        // An outpost set whose every chunk "would" start; excluded near a
        // village by 10 chunks.
        let outposts = StructureSetInfo {
            id: "minecraft:pillager_outposts".into(),
            placement: StructurePlacement::RandomSpread(RandomSpread {
                common: Common {
                    locate_offset: (0, 0, 0),
                    frequency: 1.0,
                    reduction: FrequencyReduction::Default,
                    salt: 1,
                    exclusion_zone: Some(placement::ExclusionZone {
                        other_set: "minecraft:villages".into(),
                        chunk_count: 10,
                    }),
                },
                spacing: 1,
                separation: 0,
                spread_type: SpreadType::Linear,
            }),
            structures: vec![("minecraft:pillager_outpost".into(), 1)],
            ring_positions: Vec::new(),
        };
        let reg = StructureRegistry::from_sets(vec![villages, outposts]);
        // Find the village start.
        let mut village = None;
        for cx in 0..34 {
            for cz in 0..34 {
                if reg.starts_at(42, cx, cz).iter().any(|s| s.set == 0) {
                    village = Some((cx, cz));
                }
            }
        }
        let (vx, vz) = village.expect("a village start exists");
        // The outpost is suppressed within the exclusion radius.
        let out = reg.starts_at(42, vx + 5, vz);
        assert!(out.iter().all(|s| s.set != 1));
        // And allowed well outside it.
        assert!(reg.starts_at(42, vx + 40, vz).iter().any(|s| s.set == 1));
    }

    #[test]
    fn parses_a_concentric_rings_placement_with_a_tag() {
        let j = Json::parse(
            r##"{"type":"minecraft:concentric_rings","count":128,"distance":32,
                "preferred_biomes":"#minecraft:stronghold_biased_to","salt":0,"spread":3}"##,
        )
        .unwrap();
        let holder = |v: Option<&Json>| {
            assert_eq!(v.unwrap().as_str(), Some("#minecraft:stronghold_biased_to"));
            vec![7u16, 8]
        };
        let p = StructurePlacement::parse_with(&j, &holder).unwrap();
        match p {
            StructurePlacement::ConcentricRings(p) => {
                assert_eq!((p.distance, p.spread, p.count), (32, 3, 128));
                assert_eq!(p.preferred, vec![7, 8]);
            }
            _ => panic!("wrong type"),
        }
    }
}
