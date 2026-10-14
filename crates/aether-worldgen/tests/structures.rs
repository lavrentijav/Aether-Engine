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

// --- F2: the igloo, placed from the pack's templates ----------------------

use aether_world::BlockStateId;
use aether_worldgen::vanilla::blockinfo;
use aether_worldgen::vanilla::generator::VanillaGenerator;
use aether_worldgen::ChunkGenerator;

/// Every non-air block in a generated column as `(world x, y, z, state)`.
///
/// `y` is reconstructed from the sub-chunk index and the local `y`.
fn column_blocks(
    cx: i32,
    cz: i32,
    col: &aether_worldgen::GeneratedColumn,
) -> Vec<(i32, i32, i32, BlockStateId)> {
    let mut out = Vec::new();
    for (cy, sc) in &col.sections {
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    let s = sc.get(x, y, z);
                    if s != BlockStateId::AIR {
                        out.push((
                            cx * 16 + x as i32,
                            *cy as i32 * 16 + y as i32,
                            cz * 16 + z as i32,
                            s,
                        ));
                    }
                }
            }
        }
    }
    out.sort();
    out
}

fn names(blocks: &[(i32, i32, i32, BlockStateId)]) -> Vec<&'static str> {
    blocks
        .iter()
        .map(|(_, _, _, s)| blockinfo::name(*s))
        .collect()
}

/// Seed 42's nearest igloo start in a snowy biome, found with the biome gate
/// itself (the placement alone starts igloos in any biome; the structure's own
/// `biomes` tag narrows them to snowy ones).
const IGLOO_CHUNK: (i32, i32) = (-3325, -4091);

#[test]
fn the_igloo_places_from_the_packs_templates() {
    let Some(root) = pack() else {
        eprintln!("skipping: AETHER_VANILLA_PACK not set");
        return;
    };
    let gen = VanillaGenerator::new(&root, 42).expect("build generator");
    let (cx, cz) = IGLOO_CHUNK;
    let col = gen.generate_column(cx, cz);
    let blocks = column_blocks(cx, cz, &col);
    let here = names(&blocks);

    // The above-ground half is always placed; its blocks are unique to the
    // igloo and come straight from `igloo/top.nbt`.
    assert!(
        here.contains(&"minecraft:red_bed"),
        "no igloo top in ({cx},{cz})"
    );
    assert!(here.contains(&"minecraft:snow_block"));
    assert!(here.contains(&"minecraft:white_carpet"));
    assert!(
        here.contains(&"minecraft:furnace") || here.contains(&"minecraft:crafting_table"),
        "igloo interior missing"
    );
    // The `chest` data marker was struck out, never left as a structure block.
    assert!(
        !here.contains(&"minecraft:structure_block"),
        "a structure_block marker leaked into the world"
    );

    // Deterministic: re-generating the column yields the identical blocks.
    let again = gen.generate_column(cx, cz);
    assert_eq!(blocks, column_blocks(cx, cz, &again));

    // A chunk that starts no igloo has none of its blocks.
    let empty = names(&column_blocks(0, 0, &gen.generate_column(0, 0)));
    assert!(!empty.contains(&"minecraft:red_bed"));
    assert!(!empty.contains(&"minecraft:snow_block"));
}

#[test]
fn the_igloo_lab_leaves_a_plain_empty_chest() {
    let Some(root) = pack() else {
        eprintln!("skipping: AETHER_VANILLA_PACK not set");
        return;
    };
    let gen = VanillaGenerator::new(&root, 42).expect("build generator");
    let (cx, cz) = IGLOO_CHUNK;
    let blocks = column_blocks(cx, cz, &gen.generate_column(cx, cz));
    let here = names(&blocks);
    // The 50 % laboratory roll decides whether the shaft and lab exist; when
    // the lab is there it carries the loot chest (placed empty: no block
    // entity is modelled yet) and the stone-brick shell.
    if here.contains(&"minecraft:chest") {
        assert!(here.contains(&"minecraft:stone_bricks"));
        assert!(here.contains(&"minecraft:ladder"));
    }
}
