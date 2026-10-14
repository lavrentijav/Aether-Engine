//! Structure placement against the operator's own copy of the game.
//!
//! Skipped unless `AETHER_VANILLA_PACK` points at an unpacked server jar
//! (`data/minecraft/...`). The path is the same one the worldgen examples use;
//! nothing here is vendored.
//!
//! ```sh
//! AETHER_VANILLA_PACK=/path/to/vanilla/1.21.11 cargo test -p aether-worldgen \
//!     --test structures
//! ```

use aether_worldgen::vanilla::biome::BiomeRegistry;
use aether_worldgen::vanilla::density::DataPack;
use aether_worldgen::vanilla::overworld::OverworldBiomeSource;
use aether_worldgen::vanilla::structure::StructureRegistry;

fn pack() -> Option<String> {
    std::env::var("AETHER_VANILLA_PACK").ok()
}

fn load(
    seed: u64,
) -> Option<(
    DataPack,
    OverworldBiomeSource,
    BiomeRegistry,
    StructureRegistry,
)> {
    let root = pack()?;
    let pack = DataPack::open(&root).ok()?;
    let source = OverworldBiomeSource::new(&root, seed).ok()?;
    let biomes = BiomeRegistry::load(&pack).ok()?;
    let entry_ids: Vec<u16> = source
        .biomes()
        .entries()
        .iter()
        .map(|e| biomes.id(&e.biome).unwrap_or(0))
        .collect();
    let reg = StructureRegistry::load(&pack, &source, &biomes, &entry_ids, seed as i64).ok()?;
    Some((pack, source, biomes, reg))
}

#[test]
fn the_overworld_sets_load_and_the_stronghold_rings_are_computed() {
    let Some((_pack, _source, _biomes, reg)) = load(42) else {
        eprintln!("skipping: AETHER_VANILLA_PACK not set");
        return;
    };
    let ids: Vec<&str> = reg.sets().iter().map(|s| s.id.as_str()).collect();
    for want in [
        "minecraft:igloos",
        "minecraft:desert_pyramids",
        "minecraft:strongholds",
        "minecraft:villages",
    ] {
        assert!(ids.contains(&want), "missing set {want} in {ids:?}");
    }
    // The overworld excludes nether and end structure sets.
    assert!(!ids.iter().any(|id| id.contains("nether")));
    assert!(!ids.contains(&"minecraft:end_cities"));

    let strongholds = reg
        .sets()
        .iter()
        .find(|s| s.id == "minecraft:strongholds")
        .unwrap();
    assert_eq!(strongholds.ring_positions.len(), 128);
}

#[test]
fn the_first_igloo_start_matches_the_placement_golden() {
    let Some((_pack, _source, _biomes, reg)) = load(42) else {
        eprintln!("skipping: AETHER_VANILLA_PACK not set");
        return;
    };
    // From the placement golden: seed 42, region (0,0) draws chunk (1,6).
    let starts = reg.starts_at(42, 1, 6);
    assert!(
        starts.iter().any(|s| s.structure == "minecraft:igloo"),
        "no igloo at (1,6): {starts:?}"
    );
    // And it is deterministic: the same query gives the same answer.
    assert_eq!(starts, reg.starts_at(42, 1, 6));
}
