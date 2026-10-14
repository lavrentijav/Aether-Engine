//! The igloo — a port of `IglooStructure` and `IglooPieces`.
//!
//! A template structure with a code-defined layout: always the above-ground
//! `igloo/top`, and with a 50 % chance a ladder shaft (`igloo/middle`, one to
//! eleven segments) down to a laboratory (`igloo/bottom`) with a loot chest.
//! The chest is placed empty: the loot table assignment lives in the game's
//! loot-table system, which this engine's worldgen does not touch yet.
//!
//! The pieces are positioned from a nominal height of 90 and then each snapped
//! to the surface at its own corner column, exactly as `IglooPiece.postProcess`
//! does. The template manager supplies the three `.nbt` layouts from the pack.

use std::sync::Arc;

use aether_world::registry::blocks;
use aether_world::BlockStateId;

use crate::vanilla::biome::BiomeId;
use crate::vanilla::feature::level::{Heightmap, Level, Pos};
use crate::vanilla::generator::Core;
use crate::vanilla::rng::{Rng, WorldgenRandom};

use super::template::{
    transform, Mirror, PlaceSettings, Rotation, StructureTemplate, TemplateManager,
};

/// `IglooPieces.GENERATION_HEIGHT`.
const GENERATION_HEIGHT: i32 = 90;

/// Per-template placement data from `IglooPieces.PIVOTS` / `OFFSETS`.
struct PieceKind {
    template: Arc<StructureTemplate>,
    pivot: [i32; 3],
    offset: [i32; 3],
    /// The top igloo carries the `chest` data marker and the snow lid.
    top: bool,
}

/// The three igloo templates, resolved once.
pub struct IglooStructure {
    top: PieceKind,
    middle: PieceKind,
    bottom: PieceKind,
}

impl IglooStructure {
    /// Load `igloo/top`, `igloo/middle` and `igloo/bottom`. `None` if any is
    /// missing, so a pack without them simply does not place igloos.
    pub fn load(tm: &TemplateManager) -> Option<Self> {
        Some(Self {
            top: PieceKind {
                template: tm.get("minecraft:igloo/top")?,
                pivot: [3, 5, 5],
                offset: [0, 0, 0],
                top: true,
            },
            middle: PieceKind {
                template: tm.get("minecraft:igloo/middle")?,
                pivot: [1, 3, 1],
                offset: [2, -3, 4],
                top: false,
            },
            bottom: PieceKind {
                template: tm.get("minecraft:igloo/bottom")?,
                pivot: [3, 6, 7],
                offset: [0, -3, -2],
                top: false,
            },
        })
    }

    /// Generate the igloo starting at chunk `(cx, cz)` into `lv`.
    ///
    /// Returns whether the biome at the generation position is allowed; when it
    /// is not, nothing is placed (matching `findValidGenerationPoint`).
    pub(crate) fn place(
        &self,
        core: &Core,
        lv: &mut Level,
        seed: i64,
        cx: i32,
        cz: i32,
        allowed: &dyn Fn(BiomeId) -> bool,
    ) -> bool {
        let middle_x = cx * 16 + 8;
        let middle_z = cz * 16 + 8;
        let y = lv.height(Heightmap::WorldSurfaceWg, middle_x, middle_z);
        let biome = core.noise_biome(middle_x >> 2, y >> 2, middle_z >> 2);
        if !allowed(biome) {
            return false;
        }

        let mut r = WorldgenRandom::legacy(0);
        r.set_large_feature_seed(seed, cx, cz);
        let rotation = Rotation::ALL[r.next_int_bounded(4) as usize];
        let block_pos = [cx * 16, GENERATION_HEIGHT, cz * 16];

        // `IglooPieces.addPieces`: laboratory first, then the shaft, then the
        // igloo on top.
        let mut pieces: Vec<(&PieceKind, i32)> = Vec::new();
        if r.next_double() < 0.5 {
            let n = r.next_int_bounded(8) + 4;
            pieces.push((&self.bottom, n * 3));
            for j in 0..n - 1 {
                pieces.push((&self.middle, j * 3));
            }
        }
        pieces.push((&self.top, 0));

        for (kind, y_offset) in pieces {
            self.place_piece(lv, kind, block_pos, rotation, y_offset, &mut r);
        }
        true
    }

    fn place_piece(
        &self,
        lv: &mut Level,
        kind: &PieceKind,
        block_pos: [i32; 3],
        rotation: Rotation,
        y_offset: i32,
        r: &mut WorldgenRandom,
    ) {
        let mut position = [
            block_pos[0] + kind.offset[0],
            block_pos[1] + kind.offset[1] - y_offset,
            block_pos[2] + kind.offset[2],
        ];
        // `postProcess`: snap to the surface at this piece's own corner column.
        let rel = transform(
            [3 - kind.offset[0], 0, -kind.offset[2]],
            Mirror::None,
            rotation,
            kind.pivot,
        );
        let col_x = position[0] + rel[0];
        let col_z = position[2] + rel[2];
        let surface = lv.height(Heightmap::WorldSurfaceWg, col_x, col_z);
        position[1] += surface - GENERATION_HEIGHT - 1;

        // `IglooPiece.makeSettings`: rotation about the piece pivot, no mirror,
        // and `BlockIgnoreProcessor.STRUCTURE_BLOCK` for the `chest` marker.
        kind.template.place_in_world(
            lv,
            Pos::new(position[0], position[1], position[2]),
            &PlaceSettings {
                rotation,
                mirror: Mirror::None,
                pivot: kind.pivot,
                ignore_structure_blocks: true,
                ..PlaceSettings::default()
            },
            r,
        );

        if kind.top {
            // `if block below the trapdoor is not air and not a ladder: a snow
            // block covers it`.
            let lid = transform([3, 0, 5], Mirror::None, rotation, kind.pivot);
            let lid_pos = Pos::new(
                position[0] + lid[0],
                position[1] + lid[1],
                position[2] + lid[2],
            );
            let below = lv.get(lid_pos.below(1));
            if !crate::vanilla::blockinfo::is_air(below)
                && crate::vanilla::blockinfo::name(below) != "minecraft:ladder"
            {
                lv.set(lid_pos, snow_block());
            }
        }
    }
}

fn snow_block() -> BlockStateId {
    blocks::default_state("minecraft:snow_block").unwrap_or(BlockStateId::AIR)
}
