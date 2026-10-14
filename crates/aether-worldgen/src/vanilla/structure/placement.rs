//! Vanilla structure placement — which chunks *start* a structure.
//!
//! A port of `net.minecraft.world.level.levelgen.structure.placement` for
//! 1.21.11. Structure *placement* is a pure function of the world seed and a
//! chunk position: it decides the candidate start chunks of a structure set
//! before any biome or terrain is looked at. Everything here is deterministic
//! and independent of the data pack except for how a set is *parsed* — the
//! numbers (`spacing`, `separation`, `salt`, …) always come from the pack.
//!
//! Two placement types exist:
//!
//! * [`RandomSpread`] — the common one. The world is cut into a grid of
//!   `spacing × spacing`-chunk regions; each region draws one start chunk
//!   from a `spacing - separation`-wide square inside it, seeded by
//!   [`WorldgenRandom::set_large_feature_with_salt`]. `spread_type` is either
//!   a uniform draw (`linear`) or the average of two draws (`triangular`).
//! * [`ConcentricRings`] — strongholds. No grid: positions are drawn on rings
//!   of increasing radius and then nudged to the nearest preferred biome.
//!
//! # What is exact here
//!
//! The draw sequence is transcribed from the game's own classes, so a given
//! `(seed, chunk)` yields the same candidate chunk and the same start decision
//! as vanilla. The frequency reducers (`default`, `legacy_type_1/2/3`) and the
//! exclusion zones are included because the pack uses them.
//!
//! One thing is deliberately *not* modelled here: a candidate chunk can still
//! fail to place its structure because the biome at the generation position
//! does not match the structure's `biomes` (and, for a multi-structure set, the
//! weighted draw then falls through to the next structure). That check needs
//! the terrain, so it belongs to generation, not to placement. See
//! [`super`] for the note.

use crate::vanilla::density::BuildError;
use crate::vanilla::json::Json;
use crate::vanilla::rng::{Rng, WorldgenRandom};

/// `RandomSpreadType`: how an offset within a region is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadType {
    /// `linear`: a uniform draw in `0..range`.
    Linear,
    /// `triangular`: the average of two uniform draws (a triangular density).
    Triangular,
}

impl SpreadType {
    /// `RandomSpreadType.evaluate(random, bound)`.
    pub fn evaluate(self, r: &mut WorldgenRandom, bound: i32) -> i32 {
        match self {
            SpreadType::Linear => r.next_int_bounded(bound),
            // Java integer division truncates towards zero; both draws are
            // non-negative, so this is `(a + b) / 2`.
            SpreadType::Triangular => (r.next_int_bounded(bound) + r.next_int_bounded(bound)) / 2,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "linear" => SpreadType::Linear,
            "triangular" => SpreadType::Triangular,
            _ => return None,
        })
    }
}

/// `StructurePlacement.FrequencyReductionMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrequencyReduction {
    /// `default` — one float draw salted per chunk.
    Default,
    /// `legacy_type_1` — the pillager-outpost reducer.
    LegacyType1,
    /// `legacy_type_2` — the buried-treasure reducer (arbitrary salt).
    LegacyType2,
    /// `legacy_type_3` — the mineshaft reducer (a double draw).
    LegacyType3,
}

impl FrequencyReduction {
    /// `FrequencyReductionMethod.shouldGenerate(levelSeed, salt, chunkX, chunkZ, frequency)`.
    pub fn should_generate(self, seed: i64, salt: i32, cx: i32, cz: i32, frequency: f32) -> bool {
        let mut r = WorldgenRandom::legacy(0);
        match self {
            FrequencyReduction::Default => {
                // The game passes (seed, salt, chunkX, chunkZ) straight into
                // `setLargeFeatureWithSalt(levelSeed, regionX, regionZ, salt)`.
                r.set_large_feature_with_salt(seed, salt, cx, cz);
                r.next_float() < frequency
            }
            FrequencyReduction::LegacyType1 => {
                let m = cx >> 4;
                let n = cz >> 4;
                r.set_seed(((m ^ (n << 4)) as i64) ^ seed);
                r.next_int();
                r.next_int_bounded((1.0f32 / frequency) as i32) == 0
            }
            FrequencyReduction::LegacyType2 => {
                r.set_large_feature_with_salt(seed, cx, cz, 10_387_320);
                r.next_float() < frequency
            }
            FrequencyReduction::LegacyType3 => {
                r.set_large_feature_seed(seed, cx, cz);
                r.next_double() < frequency as f64
            }
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "default" => FrequencyReduction::Default,
            "legacy_type_1" => FrequencyReduction::LegacyType1,
            "legacy_type_2" => FrequencyReduction::LegacyType2,
            "legacy_type_3" => FrequencyReduction::LegacyType3,
            _ => return None,
        })
    }
}

/// `StructurePlacement.ExclusionZone`: do not place within `chunk_count` chunks
/// of another structure set's start chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExclusionZone {
    /// The id of the other structure set.
    pub other_set: String,
    /// `chunk_count`.
    pub chunk_count: i32,
}

/// The fields every placement shares.
#[derive(Debug, Clone, PartialEq)]
pub struct Common {
    /// `locate_offset` — only affects `/locate`, kept for completeness.
    pub locate_offset: (i32, i32, i32),
    /// `frequency`.
    pub frequency: f32,
    /// `frequency_reduction_method`.
    pub reduction: FrequencyReduction,
    /// `salt`.
    pub salt: i32,
    /// `exclusion_zone`.
    pub exclusion_zone: Option<ExclusionZone>,
}

impl Common {
    fn parse(j: &Json) -> Self {
        let locate_offset = j
            .get("locate_offset")
            .and_then(Json::as_arr)
            .and_then(|a| {
                Some((
                    a.first()?.as_f64()? as i32,
                    a.get(1)?.as_f64()? as i32,
                    a.get(2)?.as_f64()? as i32,
                ))
            })
            .unwrap_or((0, 0, 0));
        let reduction = j
            .str_of("frequency_reduction_method")
            .and_then(FrequencyReduction::parse)
            .unwrap_or(FrequencyReduction::Default);
        let frequency = j.f64_or("frequency", 1.0) as f32;
        let salt = j.i32_or("salt", 0);
        let exclusion_zone = j.get("exclusion_zone").and_then(|z| {
            Some(ExclusionZone {
                other_set: z.str_of("other_set")?.to_string(),
                chunk_count: z.i32_or("chunk_count", 0),
            })
        });
        Self {
            locate_offset,
            frequency,
            reduction,
            salt,
            exclusion_zone,
        }
    }

    /// `applyAdditionalChunkRestrictions`: the frequency gate, short-circuited
    /// when the frequency is 1.
    pub fn allows(&self, seed: i64, cx: i32, cz: i32) -> bool {
        self.frequency >= 1.0
            || self
                .reduction
                .should_generate(seed, self.salt, cx, cz, self.frequency)
    }
}

/// `RandomSpreadStructurePlacement`.
#[derive(Debug, Clone, PartialEq)]
pub struct RandomSpread {
    /// Shared fields.
    pub common: Common,
    /// `spacing`.
    pub spacing: i32,
    /// `separation`.
    pub separation: i32,
    /// `spread_type`.
    pub spread_type: SpreadType,
}

impl RandomSpread {
    /// `getPotentialStructureChunk(seed, chunkX, chunkZ)`.
    pub fn potential_chunk(&self, seed: i64, cx: i32, cz: i32) -> (i32, i32) {
        let k = cx.div_euclid(self.spacing);
        let m = cz.div_euclid(self.spacing);
        let mut r = WorldgenRandom::legacy(0);
        r.set_large_feature_with_salt(seed, k, m, self.common.salt);
        let n = self.spacing - self.separation;
        let o = self.spread_type.evaluate(&mut r, n);
        let p = self.spread_type.evaluate(&mut r, n);
        (k * self.spacing + o, m * self.spacing + p)
    }

    /// `isPlacementChunk`: is this chunk the region's drawn start chunk?
    pub fn is_placement_chunk(&self, seed: i64, cx: i32, cz: i32) -> bool {
        self.potential_chunk(seed, cx, cz) == (cx, cz)
    }

    /// `isStructureChunk` without the exclusion-zone interaction (that needs
    /// the other set) — the placement grid plus the frequency gate.
    pub fn is_start_chunk(&self, seed: i64, cx: i32, cz: i32) -> bool {
        self.is_placement_chunk(seed, cx, cz) && self.common.allows(seed, cx, cz)
    }
}

/// `ConcentricRingsStructurePlacement` (strongholds).
#[derive(Debug, Clone, PartialEq)]
pub struct ConcentricRings {
    /// Shared fields.
    pub common: Common,
    /// `distance` — ring spacing constant, in 16-block units.
    pub distance: i32,
    /// `spread` — how many attempts per ring before the ring grows.
    pub spread: i32,
    /// `count` — total number of strongholds.
    pub count: i32,
    /// `preferred_biomes`, resolved to registry ids.
    pub preferred: Vec<u16>,
}

/// The result of the stronghold search: a chunk position.
pub type ChunkPos = (i32, i32);

impl ConcentricRings {
    /// `generateRingPositions`: the final chunk of each stronghold.
    ///
    /// `biome_at` is the *raw* biome source at a quart position (the game's
    /// `getNoiseBiome`), and `preferred` answers whether that biome is one of
    /// the placement's preferred biomes. The biome search is the rectangular
    /// spiral of `BiomeSource.findBiomeHorizontal` with the reservoir draw it
    /// uses to pick among equally-near candidates.
    pub fn ring_positions(
        &self,
        seed: i64,
        biome_at: &dyn Fn(i32, i32, i32) -> u16,
        preferred: &dyn Fn(u16) -> bool,
    ) -> Vec<ChunkPos> {
        if self.count == 0 {
            return Vec::new();
        }
        let i = self.distance;
        let j = self.count;
        let mut k = self.spread;
        let mut r = WorldgenRandom::legacy(seed);
        let mut d = r.next_double() * std::f64::consts::PI * 2.0;
        let mut l = 0;
        let mut m = 0;
        let mut out = Vec::with_capacity(j as usize);
        for n in 0..j {
            let e = (4 * i + i * m * 6) as f64 + (r.next_double() - 0.5) * (i as f64 * 2.5);
            let o = (d.cos() * e).round() as i32;
            let p = (d.sin() * e).round() as i32;
            // `randomSource.fork()` = a fresh LegacyRandomSource over nextLong().
            let mut r2 = WorldgenRandom::legacy(r.next_long());
            let found =
                find_biome_horizontal(biome_at, preferred, o * 16 + 8, 0, p * 16 + 8, 112, &mut r2);
            out.push(match found {
                Some((bx, bz)) => (bx >> 4, bz >> 4),
                None => (o, p),
            });
            d += (std::f64::consts::PI * 2.0) / k as f64;
            l += 1;
            if l == k {
                m += 1;
                l = 0;
                k += 2 * k / (m + 1);
                k = k.min(j - n);
                d += r.next_double() * std::f64::consts::PI * 2.0;
            }
        }
        out
    }

    /// `isPlacementChunk`: membership in the (precomputed) ring positions.
    pub fn is_start_chunk_in(&self, positions: &[ChunkPos], cx: i32, cz: i32) -> bool {
        positions.contains(&(cx, cz))
    }
}

/// `BiomeSource.findBiomeHorizontal(blockX, blockY, blockZ, radius, predicate, random, sampler)`.
///
/// The 9-argument overload with `step = 1` and `findClosest = false`. Returns
/// the block position of a chosen matching column, or `None`.
fn find_biome_horizontal(
    biome_at: &dyn Fn(i32, i32, i32) -> u16,
    preferred: &dyn Fn(u16) -> bool,
    block_x: i32,
    block_y: i32,
    block_z: i32,
    radius: i32,
    r: &mut WorldgenRandom,
) -> Option<(i32, i32)> {
    let n = quart(block_x);
    let o = quart(block_z);
    let p = quart(radius);
    let q = quart(block_y);
    let mut found: Option<(i32, i32)> = None;
    let mut count = 0i32;
    // `findClosest` is false here, so the game sets `t = p` and walks the
    // whole square once: `u` (Z) over `-p..=p`, `v` (X) over `-p..=p`.
    let t = p;
    let mut u = -t;
    while u <= t {
        let mut v = -t;
        while v <= t {
            let w = n + v;
            let x = o + u;
            if preferred(biome_at(w, q, x)) {
                if found.is_none() || r.next_int_bounded(count + 1) == 0 {
                    found = Some((to_block(w), to_block(x)));
                }
                count += 1;
            }
            v += 1;
        }
        u += 1;
    }
    found
}

/// `QuartPos.fromBlock`.
fn quart(b: i32) -> i32 {
    b >> 2
}

/// `QuartPos.toBlock`.
fn to_block(q: i32) -> i32 {
    q << 2
}

/// `StructurePlacement`, parsed.
#[derive(Debug, Clone, PartialEq)]
pub enum StructurePlacement {
    /// `random_spread`.
    RandomSpread(RandomSpread),
    /// `concentric_rings`.
    ConcentricRings(ConcentricRings),
}

impl StructurePlacement {
    /// Parse a placement object.
    ///
    /// `holder_set` resolves a `HolderSet<Biome>` (`"#tag"`, a name, or a list)
    /// to registry ids.
    pub fn parse_with(
        j: &Json,
        holder_set: &dyn Fn(Option<&Json>) -> Vec<u16>,
    ) -> Result<Self, BuildError> {
        let ty = j
            .str_of("type")
            .unwrap_or("")
            .trim_start_matches("minecraft:");
        match ty {
            "random_spread" => {
                let spacing = j.i32_or("spacing", 0);
                let separation = j.i32_or("separation", 0);
                if spacing <= separation {
                    return Err(BuildError::new(
                        "structure_set: spacing has to be larger than separation",
                    ));
                }
                let spread_type = j
                    .str_of("spread_type")
                    .and_then(SpreadType::parse)
                    .unwrap_or(SpreadType::Linear);
                Ok(StructurePlacement::RandomSpread(RandomSpread {
                    common: Common::parse(j),
                    spacing,
                    separation,
                    spread_type,
                }))
            }
            "concentric_rings" => Ok(StructurePlacement::ConcentricRings(ConcentricRings {
                common: Common::parse(j),
                distance: j.i32_or("distance", 0),
                spread: j.i32_or("spread", 3),
                count: j.i32_or("count", 0),
                preferred: holder_set(j.get("preferred_biomes")),
            })),
            other => Err(BuildError::new(format!(
                "unknown structure placement type `{other}`"
            ))),
        }
    }

    /// The shared fields.
    pub fn common(&self) -> &Common {
        match self {
            StructurePlacement::RandomSpread(p) => &p.common,
            StructurePlacement::ConcentricRings(p) => &p.common,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn igloo() -> RandomSpread {
        RandomSpread {
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
        }
    }

    #[test]
    fn the_region_offset_lies_inside_the_region() {
        let p = igloo();
        for cx in -200..200 {
            for cz in -200..200 {
                let (px, pz) = p.potential_chunk(42, cx, cz);
                // The drawn chunk is always within the same region.
                assert_eq!(px.div_euclid(32), cx.div_euclid(32));
                assert_eq!(pz.div_euclid(32), cz.div_euclid(32));
                // Every start is inside the `spacing - separation` square.
                let dx = px.rem_euclid(32);
                let dz = pz.rem_euclid(32);
                assert!((0..24).contains(&dx), "x offset {dx} out of range");
                assert!((0..24).contains(&dz), "z offset {dz} out of range");
            }
        }
    }

    #[test]
    fn exactly_one_start_per_region() {
        // Over a whole region there is exactly one chunk the placement names.
        let p = igloo();
        let mut starts = 0;
        for cx in 0..32 {
            for cz in 0..32 {
                if p.is_placement_chunk(42, cx, cz) {
                    starts += 1;
                }
            }
        }
        assert_eq!(starts, 1);
    }

    #[test]
    fn placement_is_deterministic() {
        let p = igloo();
        for &seed in &[0i64, 1, -1, 42, 123_456_789, i64::MIN + 1] {
            for &(cx, cz) in &[(0, 0), (-1, -1), (17, -33), (1_000_000, -2_000_000)] {
                assert_eq!(
                    p.potential_chunk(seed, cx, cz),
                    p.potential_chunk(seed, cx, cz)
                );
            }
        }
    }

    #[test]
    fn goldens_match_the_game() {
        // Expected values printed by a standalone Java port of
        // `RandomSpreadStructurePlacement` (java.util.Random's own LCG) with
        // igloo's own numbers: spacing 32, separation 8, salt 14357618.
        let p = igloo();
        let cases: [(i64, i32, i32, i32, i32); 6] = [
            (0, 0, 0, 14, 10),
            (42, 0, 0, 1, 6),
            (42, 5, -7, 10, -9),
            (123456789, 100, 200, 118, 204),
            (-987654321, -33, 17, -62, 8),
            (1, 1_000_000, -2_000_000, 1_000_020, -1_999_998),
        ];
        for (seed, cx, cz, ex, ez) in cases {
            assert_eq!(
                p.potential_chunk(seed, cx, cz),
                (ex, ez),
                "seed {seed} chunk ({cx},{cz})"
            );
        }
    }

    #[test]
    fn triangular_stays_in_range() {
        let p = RandomSpread {
            spread_type: SpreadType::Triangular,
            ..igloo()
        };
        for cx in -100..100 {
            for cz in -100..100 {
                let (px, pz) = p.potential_chunk(7, cx, cz);
                assert!((0..24).contains(&px.rem_euclid(32)));
                assert!((0..24).contains(&pz.rem_euclid(32)));
            }
        }
    }

    #[test]
    fn frequency_one_always_allows() {
        let c = Common {
            locate_offset: (0, 0, 0),
            frequency: 1.0,
            reduction: FrequencyReduction::Default,
            salt: 123,
            exclusion_zone: None,
        };
        for cx in -20..20 {
            assert!(c.allows(99, cx, cx));
        }
    }

    #[test]
    fn frequency_reducers_are_deterministic_and_gated() {
        // A frequency below 1 has to reject some chunks and accept others,
        // and never depend on anything but its inputs.
        for (red, freq) in [
            (FrequencyReduction::Default, 0.1f32),
            (FrequencyReduction::LegacyType1, 0.2),
            (FrequencyReduction::LegacyType2, 0.01),
            (FrequencyReduction::LegacyType3, 0.004),
        ] {
            let mut yes = 0;
            let mut no = 0;
            for cx in 0..400 {
                let a = red.should_generate(42, 0, cx, 0, freq);
                let b = red.should_generate(42, 0, cx, 0, freq);
                assert_eq!(a, b, "non-deterministic reducer {red:?}");
                if a {
                    yes += 1;
                } else {
                    no += 1;
                }
            }
            assert!(yes > 0 && no > 0, "reducer {red:?} never varies at {freq}");
        }
    }

    #[test]
    fn concentric_rings_land_on_preferred_biomes() {
        // A fake world where biome 1 is the preferred one everywhere, so the
        // search always finds a column and every ring lands on a real biome.
        let p = ConcentricRings {
            common: Common {
                locate_offset: (0, 0, 0),
                frequency: 1.0,
                reduction: FrequencyReduction::Default,
                salt: 0,
                exclusion_zone: None,
            },
            distance: 32,
            spread: 3,
            count: 8,
            preferred: vec![1],
        };
        let biome = |_x: i32, _y: i32, _z: i32| 1u16;
        let pref = |b: u16| b == 1;
        let a = p.ring_positions(42, &biome, &pref);
        let b = p.ring_positions(42, &biome, &pref);
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        // Distinct positions spread around the origin.
        let mut sorted = a.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), a.len());
    }

    #[test]
    fn concentric_rings_have_no_biome_fallback() {
        // No biome matches: every position is the raw ring point, unaffected
        // by the search.
        let p = ConcentricRings {
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
        };
        let biome = |_x: i32, _y: i32, _z: i32| 2u16;
        let pref = |b: u16| b == 1;
        let positions = p.ring_positions(42, &biome, &pref);
        assert_eq!(positions.len(), 4);
    }

    #[test]
    fn parses_a_real_placement() {
        let j = Json::parse(
            r#"{"type":"minecraft:random_spread","salt":14357618,"separation":8,"spacing":32}"#,
        )
        .unwrap();
        let p = StructurePlacement::parse_with(&j, &|_| Vec::new()).unwrap();
        match p {
            StructurePlacement::RandomSpread(p) => {
                assert_eq!(
                    (p.spacing, p.separation, p.common.salt),
                    (32, 8, 14_357_618)
                );
                assert_eq!(p.spread_type, SpreadType::Linear);
                assert_eq!(p.common.frequency, 1.0);
            }
            _ => panic!("wrong type"),
        }
    }
}
