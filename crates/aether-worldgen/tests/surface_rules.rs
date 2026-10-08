//! Surface rules against a real copy of the game's worldgen data.
//!
//! Same gating as `tests/vanilla_parity.rs`: point the environment at an
//! unpacked jar and a biome-parameter report and these run; leave it unset
//! and they skip, so `cargo test` stays green with none of it on disk.
//!
//! ```text
//! AETHER_VANILLA_PACK=/path/to/unpacked-jar \
//! AETHER_VANILLA_BIOME_REPORT=/path/to/reports/biome_parameters/minecraft/overworld.json \
//! cargo test -p aether-worldgen --test surface_rules -- --nocapture
//! ```

use aether_world::registry::{blocks, BlockRegistry};
use aether_world::BlockStateId;
use aether_worldgen::vanilla::generator::VanillaGenerator;
use aether_worldgen::ChunkGenerator;

struct Env {
    pack: String,
    report: String,
}

fn env() -> Option<Env> {
    Some(Env {
        pack: std::env::var("AETHER_VANILLA_PACK").ok()?,
        report: std::env::var("AETHER_VANILLA_BIOME_REPORT").ok()?,
    })
}

fn id(name: &str) -> BlockStateId {
    blocks::default_state(name).unwrap_or_else(|| panic!("registry has no `{name}`"))
}

/// One column's blocks, `min_y..min_y+height`, resolved to a name (or
/// `"air"`), bottom to top.
fn column_names(gen: &VanillaGenerator, x: i32, z: i32) -> Vec<BlockStateId> {
    let cx = x.div_euclid(16);
    let cz = z.div_euclid(16);
    let lx = x.rem_euclid(16) as usize;
    let lz = z.rem_euclid(16) as usize;
    let col = gen.generate_column(cx, cz);
    let s = gen.terrain().settings();
    let air = id("minecraft:air");
    let mut out = vec![air; s.height as usize];
    for (cy, sc) in &col.sections {
        for ly in 0..16usize {
            let world_y = *cy as i32 * 16 + ly as i32;
            let i = world_y - s.min_y;
            if i >= 0 && (i as usize) < out.len() {
                out[i as usize] = sc.get(lx, ly, lz);
            }
        }
    }
    out
}

/// The topmost solid (non-air, non-fluid) block and its Y — the seabed under
/// an ocean, or dry ground where there is no water.
fn top(gen: &VanillaGenerator, x: i32, z: i32) -> (i32, BlockStateId) {
    let s = gen.terrain().settings();
    let names = column_names(gen, x, z);
    let air = id("minecraft:air");
    let lava = id("minecraft:lava");
    let registry = BlockRegistry::new();
    for (i, b) in names.iter().enumerate().rev() {
        if *b == air || *b == lava {
            continue;
        }
        // Any water level state, not just the source block's default.
        if registry.block_name_of(*b) == Some("minecraft:water") {
            continue;
        }
        return (s.min_y + i as i32, *b);
    }
    panic!("column at ({x},{z}) is entirely air/fluid");
}

#[test]
fn bedrock_is_at_the_bottom_of_every_column() {
    let Some(e) = env() else {
        eprintln!("skipping: AETHER_VANILLA_* not set");
        return;
    };
    let gen = VanillaGenerator::load(&e.pack, &e.report, 12345).expect("generator loads");
    let bedrock = id("minecraft:bedrock");
    let s = gen.terrain().settings();
    let mut saw_bedrock = false;
    for x in [0, 37, -80, 512] {
        for z in [0, -22, 300] {
            let names = column_names(&gen, x, z);
            // Vanilla's bedrock floor is a seeded 0-4 gradient above min_y,
            // so min_y itself is bedrock with overwhelming (not 100%)
            // probability; min_y is always at least a candidate.
            if names[0] == bedrock {
                saw_bedrock = true;
            }
            // Above the gradient's top (min_y + 5) nothing should be
            // bedrock — a stray placement there would mean the gradient's
            // anchors are wrong.
            let above_gradient = 5;
            for (i, b) in names.iter().enumerate().skip(above_gradient) {
                assert_ne!(
                    *b,
                    bedrock,
                    "bedrock above the gradient at ({x},{},{z})",
                    s.min_y + i as i32
                );
            }
        }
    }
    assert!(
        saw_bedrock,
        "no column had bedrock at min_y across a spread of columns"
    );
}

#[test]
fn deepslate_replaces_stone_below_its_gradient_and_not_above() {
    let Some(e) = env() else {
        eprintln!("skipping: AETHER_VANILLA_* not set");
        return;
    };
    let gen = VanillaGenerator::load(&e.pack, &e.report, 12345).expect("generator loads");
    let stone = id("minecraft:stone");
    let deepslate = id("minecraft:deepslate");
    let s = gen.terrain().settings();

    let mut saw_deepslate = false;
    let mut saw_stone_above = false;
    for x in [0, 37, -80, 512] {
        for z in [0, -22, 300] {
            let names = column_names(&gen, x, z);
            // Well below the gradient (true_at_and_below: absolute 0):
            // any stone-family block down there should have become
            // deepslate.
            for y in (s.min_y..-20).step_by(7) {
                let i = (y - s.min_y) as usize;
                if names[i] == stone {
                    panic!("plain stone survived at y={y}, below the deepslate gradient");
                }
                if names[i] == deepslate {
                    saw_deepslate = true;
                }
            }
            // Well above it (false_at_and_above: absolute 8): stone stays
            // stone.
            for y in (40..s.min_y + s.height).step_by(11) {
                let i = (y - s.min_y) as usize;
                if names[i] == stone {
                    saw_stone_above = true;
                }
                assert_ne!(names[i], deepslate, "deepslate above its gradient at y={y}");
            }
        }
    }
    assert!(saw_deepslate, "never saw deepslate below the gradient");
    assert!(saw_stone_above, "never saw plain stone above the gradient");
}

#[test]
fn a_dry_land_column_is_grass_over_dirt_over_stone() {
    let Some(e) = env() else {
        eprintln!("skipping: AETHER_VANILLA_* not set");
        return;
    };
    let gen = VanillaGenerator::load(&e.pack, &e.report, 12345).expect("generator loads");
    let grass = id("minecraft:grass_block");
    let dirt = id("minecraft:dirt");
    let coarse_dirt = id("minecraft:coarse_dirt");
    let podzol = id("minecraft:podzol");
    let stone = id("minecraft:stone");
    let deepslate = id("minecraft:deepslate");
    let sea = gen.terrain().settings().sea_level;

    // Scan a spread of columns for one that lands above sea level with grass
    // on top — this is a search, not a fixed coordinate, because which
    // column that is depends on the seed's terrain and biome placement.
    let mut found = false;
    'search: for cx in -8..8 {
        for cz in -8..8 {
            let x = cx * 16 + 8;
            let z = cz * 16 + 8;
            let (y, b) = top(&gen, x, z);
            if y > sea && b == grass {
                let names = column_names(&gen, x, z);
                let i = (y - gen.terrain().settings().min_y) as usize;
                // Immediately under grass: dirt (or one of its biome
                // variants) for a few blocks, then stone/deepslate beneath.
                assert!(
                    names[i - 1] == dirt || names[i - 1] == coarse_dirt || names[i - 1] == podzol,
                    "expected dirt under grass at ({x},{y},{z}), got {:?}",
                    names[i - 1]
                );
                let deep = names[i - 20.min(i)];
                assert!(
                    deep == stone || deep == deepslate,
                    "expected stone/deepslate well under grass, got {deep:?}"
                );
                found = true;
                break 'search;
            }
        }
    }
    assert!(found, "no dry grass column found in the sampled range");
}

#[test]
fn an_underwater_column_gets_sand_or_gravel_not_grass() {
    let Some(e) = env() else {
        eprintln!("skipping: AETHER_VANILLA_* not set");
        return;
    };
    let gen = VanillaGenerator::load(&e.pack, &e.report, 12345).expect("generator loads");
    let grass = id("minecraft:grass_block");
    let sand = id("minecraft:sand");
    let red_sand = id("minecraft:red_sand");
    let gravel = id("minecraft:gravel");
    let sandstone = id("minecraft:sandstone");
    let sea = gen.terrain().settings().sea_level;

    // The unconditional half of this: no seabed ever gets grass — vanilla's
    // own tree gates grass on the `water` condition being true (dry), and a
    // wrong reconstruction of that condition would show up here as grass
    // under water.
    //
    // The positive half is a search, not a fixed coordinate: which biome (and
    // so which of sand / gravel / sandstone / dirt — regular, non-warm ocean
    // floors are legitimately dirt/gravel in vanilla too) a given underwater
    // column gets depends on where this seed puts warm/lukewarm ocean or
    // beach. So this only requires finding *one* sand-family seabed
    // somewhere in the sampled range, proving the beach/warm-ocean branch of
    // the tree actually fires.
    let mut saw_underwater = false;
    let mut saw_sand_family = false;
    for cx in -8..8 {
        for cz in -8..8 {
            let x = cx * 16 + 8;
            let z = cz * 16 + 8;
            let (y, b) = top(&gen, x, z);
            if y <= sea {
                saw_underwater = true;
                assert_ne!(b, grass, "grass under water at ({x},{y},{z})");
                if b == sand || b == red_sand || b == gravel || b == sandstone {
                    saw_sand_family = true;
                }
            }
        }
    }
    assert!(
        saw_underwater,
        "no underwater column found in the sampled range"
    );
    assert!(
        saw_sand_family,
        "no sand/gravel/sandstone seabed found in the sampled range"
    );
}
