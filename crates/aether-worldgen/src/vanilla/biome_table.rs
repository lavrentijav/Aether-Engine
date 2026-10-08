//! The overworld's multi-noise biome table, rebuilt in code.
//!
//! In the game this table is not data: `OverworldBiomeBuilder` constructs it
//! procedurally at start-up from a handful of threshold arrays and lookup
//! grids, and the only place it ever reaches disk is the `--reports` dump.
//! This module is a line-for-line port of that builder, so the generator no
//! longer needs the report.
//!
//! Order matters as much as content. The search tree is bulk-loaded from the
//! rows *in insertion order*, and the tree's shape decides which of two
//! equidistant biomes wins a tie (see [`super::climate`]). Every `add_*` call
//! below therefore happens in the same sequence as the game's, and
//! `tests/vanilla_parity.rs` (`biome_table_matches_report`) compares the result
//! row by row against a report when one is available.

use super::climate::{quantize, Parameter, ParameterPoint};

/// `Climate.Parameter.span(min, max)`.
fn span(min: f32, max: f32) -> Parameter {
    Parameter {
        min: quantize(min),
        max: quantize(max),
    }
}

/// `Climate.Parameter.span(a, b)` over two parameters: `a.min .. b.max`.
fn join(a: Parameter, b: Parameter) -> Parameter {
    Parameter {
        min: a.min,
        max: b.max,
    }
}

fn point(v: f32) -> Parameter {
    span(v, v)
}

const SNOWY_PLAINS: &str = "minecraft:snowy_plains";
const SNOWY_TAIGA: &str = "minecraft:snowy_taiga";
const TAIGA: &str = "minecraft:taiga";
const PLAINS: &str = "minecraft:plains";
const FOREST: &str = "minecraft:forest";
const OLD_GROWTH_SPRUCE_TAIGA: &str = "minecraft:old_growth_spruce_taiga";
const FLOWER_FOREST: &str = "minecraft:flower_forest";
const BIRCH_FOREST: &str = "minecraft:birch_forest";
const DARK_FOREST: &str = "minecraft:dark_forest";
const SAVANNA: &str = "minecraft:savanna";
const JUNGLE: &str = "minecraft:jungle";
const DESERT: &str = "minecraft:desert";
const ICE_SPIKES: &str = "minecraft:ice_spikes";
const OLD_GROWTH_PINE_TAIGA: &str = "minecraft:old_growth_pine_taiga";
const SUNFLOWER_PLAINS: &str = "minecraft:sunflower_plains";
const OLD_GROWTH_BIRCH_FOREST: &str = "minecraft:old_growth_birch_forest";
const SPARSE_JUNGLE: &str = "minecraft:sparse_jungle";
const BAMBOO_JUNGLE: &str = "minecraft:bamboo_jungle";
const MEADOW: &str = "minecraft:meadow";
const PALE_GARDEN: &str = "minecraft:pale_garden";
const SAVANNA_PLATEAU: &str = "minecraft:savanna_plateau";
const BADLANDS: &str = "minecraft:badlands";
const WOODED_BADLANDS: &str = "minecraft:wooded_badlands";
const ERODED_BADLANDS: &str = "minecraft:eroded_badlands";
const CHERRY_GROVE: &str = "minecraft:cherry_grove";
const WINDSWEPT_GRAVELLY_HILLS: &str = "minecraft:windswept_gravelly_hills";
const WINDSWEPT_HILLS: &str = "minecraft:windswept_hills";
const WINDSWEPT_FOREST: &str = "minecraft:windswept_forest";
const WINDSWEPT_SAVANNA: &str = "minecraft:windswept_savanna";
const SNOWY_BEACH: &str = "minecraft:snowy_beach";
const BEACH: &str = "minecraft:beach";
const JAGGED_PEAKS: &str = "minecraft:jagged_peaks";
const FROZEN_PEAKS: &str = "minecraft:frozen_peaks";
const STONY_PEAKS: &str = "minecraft:stony_peaks";
const SNOWY_SLOPES: &str = "minecraft:snowy_slopes";
const GROVE: &str = "minecraft:grove";
const STONY_SHORE: &str = "minecraft:stony_shore";
const SWAMP: &str = "minecraft:swamp";
const MANGROVE_SWAMP: &str = "minecraft:mangrove_swamp";
const FROZEN_RIVER: &str = "minecraft:frozen_river";
const RIVER: &str = "minecraft:river";
const MUSHROOM_FIELDS: &str = "minecraft:mushroom_fields";
const DRIPSTONE_CAVES: &str = "minecraft:dripstone_caves";
const LUSH_CAVES: &str = "minecraft:lush_caves";
const DEEP_DARK: &str = "minecraft:deep_dark";

const OCEANS: [[&str; 5]; 2] = [
    [
        "minecraft:deep_frozen_ocean",
        "minecraft:deep_cold_ocean",
        "minecraft:deep_ocean",
        "minecraft:deep_lukewarm_ocean",
        "minecraft:warm_ocean",
    ],
    [
        "minecraft:frozen_ocean",
        "minecraft:cold_ocean",
        "minecraft:ocean",
        "minecraft:lukewarm_ocean",
        "minecraft:warm_ocean",
    ],
];

type Grid = [[Option<&'static str>; 5]; 5];

const MIDDLE_BIOMES: [[&str; 5]; 5] = [
    [SNOWY_PLAINS, SNOWY_PLAINS, SNOWY_PLAINS, SNOWY_TAIGA, TAIGA],
    [PLAINS, PLAINS, FOREST, TAIGA, OLD_GROWTH_SPRUCE_TAIGA],
    [FLOWER_FOREST, PLAINS, FOREST, BIRCH_FOREST, DARK_FOREST],
    [SAVANNA, SAVANNA, FOREST, JUNGLE, JUNGLE],
    [DESERT, DESERT, DESERT, DESERT, DESERT],
];

const MIDDLE_BIOMES_VARIANT: Grid = [
    [Some(ICE_SPIKES), None, Some(SNOWY_TAIGA), None, None],
    [None, None, None, None, Some(OLD_GROWTH_PINE_TAIGA)],
    [Some(SUNFLOWER_PLAINS), None, None, Some(OLD_GROWTH_BIRCH_FOREST), None],
    [None, None, Some(PLAINS), Some(SPARSE_JUNGLE), Some(BAMBOO_JUNGLE)],
    [None, None, None, None, None],
];

const PLATEAU_BIOMES: [[&str; 5]; 5] = [
    [SNOWY_PLAINS, SNOWY_PLAINS, SNOWY_PLAINS, SNOWY_TAIGA, SNOWY_TAIGA],
    [MEADOW, MEADOW, FOREST, TAIGA, OLD_GROWTH_SPRUCE_TAIGA],
    [MEADOW, MEADOW, MEADOW, MEADOW, PALE_GARDEN],
    [SAVANNA_PLATEAU, SAVANNA_PLATEAU, FOREST, FOREST, JUNGLE],
    [BADLANDS, BADLANDS, BADLANDS, WOODED_BADLANDS, WOODED_BADLANDS],
];

const PLATEAU_BIOMES_VARIANT: Grid = [
    [Some(ICE_SPIKES), None, None, None, None],
    [Some(CHERRY_GROVE), None, Some(MEADOW), Some(MEADOW), Some(OLD_GROWTH_PINE_TAIGA)],
    [Some(CHERRY_GROVE), Some(CHERRY_GROVE), Some(FOREST), Some(BIRCH_FOREST), None],
    [None, None, None, None, None],
    [Some(ERODED_BADLANDS), Some(ERODED_BADLANDS), None, None, None],
];

const SHATTERED_BIOMES: Grid = [
    [
        Some(WINDSWEPT_GRAVELLY_HILLS),
        Some(WINDSWEPT_GRAVELLY_HILLS),
        Some(WINDSWEPT_HILLS),
        Some(WINDSWEPT_FOREST),
        Some(WINDSWEPT_FOREST),
    ],
    [
        Some(WINDSWEPT_GRAVELLY_HILLS),
        Some(WINDSWEPT_GRAVELLY_HILLS),
        Some(WINDSWEPT_HILLS),
        Some(WINDSWEPT_FOREST),
        Some(WINDSWEPT_FOREST),
    ],
    [
        Some(WINDSWEPT_HILLS),
        Some(WINDSWEPT_HILLS),
        Some(WINDSWEPT_HILLS),
        Some(WINDSWEPT_FOREST),
        Some(WINDSWEPT_FOREST),
    ],
    [None, None, None, None, None],
    [None, None, None, None, None],
];

struct Builder {
    full: Parameter,
    temperatures: [Parameter; 5],
    humidities: [Parameter; 5],
    erosions: [Parameter; 7],
    frozen: Parameter,
    unfrozen: Parameter,
    mushroom: Parameter,
    deep_ocean: Parameter,
    ocean: Parameter,
    coast: Parameter,
    inland: Parameter,
    near_inland: Parameter,
    mid_inland: Parameter,
    far_inland: Parameter,
    out: Vec<ParameterPoint>,
}

impl Builder {
    fn new() -> Self {
        let temperatures = [
            span(-1.0, -0.45),
            span(-0.45, -0.15),
            span(-0.15, 0.2),
            span(0.2, 0.55),
            span(0.55, 1.0),
        ];
        Self {
            full: span(-1.0, 1.0),
            temperatures,
            humidities: [
                span(-1.0, -0.35),
                span(-0.35, -0.1),
                span(-0.1, 0.1),
                span(0.1, 0.3),
                span(0.3, 1.0),
            ],
            erosions: [
                span(-1.0, -0.78),
                span(-0.78, -0.375),
                span(-0.375, -0.2225),
                span(-0.2225, 0.05),
                span(0.05, 0.45),
                span(0.45, 0.55),
                span(0.55, 1.0),
            ],
            frozen: temperatures[0],
            unfrozen: join(temperatures[1], temperatures[4]),
            mushroom: span(-1.2, -1.05),
            deep_ocean: span(-1.05, -0.455),
            ocean: span(-0.455, -0.19),
            coast: span(-0.19, -0.11),
            inland: span(-0.11, 0.55),
            near_inland: span(-0.11, 0.03),
            mid_inland: span(0.03, 0.3),
            far_inland: span(0.3, 1.0),
            out: Vec::with_capacity(8000),
        }
    }

    fn push(&mut self, t: Parameter, h: Parameter, c: Parameter, e: Parameter, d: Parameter, w: Parameter, offset: f32, biome: &str) {
        self.out.push(ParameterPoint {
            biome: biome.to_string(),
            space: [t, h, c, e, d, w],
            offset: quantize(offset),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn surface(&mut self, t: Parameter, h: Parameter, c: Parameter, e: Parameter, w: Parameter, offset: f32, biome: &str) {
        self.push(t, h, c, e, point(0.0), w, offset, biome);
        self.push(t, h, c, e, point(1.0), w, offset, biome);
    }

    #[allow(clippy::too_many_arguments)]
    fn underground(&mut self, t: Parameter, h: Parameter, c: Parameter, e: Parameter, w: Parameter, offset: f32, biome: &str) {
        self.push(t, h, c, e, span(0.2, 0.9), w, offset, biome);
    }

    #[allow(clippy::too_many_arguments)]
    fn bottom(&mut self, t: Parameter, h: Parameter, c: Parameter, e: Parameter, w: Parameter, offset: f32, biome: &str) {
        self.push(t, h, c, e, point(1.1), w, offset, biome);
    }

    fn add_biomes(&mut self) {
        self.add_off_coast();
        self.add_inland();
        self.add_underground();
    }

    fn add_off_coast(&mut self) {
        let f = self.full;
        self.surface(f, f, self.mushroom, f, f, 0.0, MUSHROOM_FIELDS);
        for i in 0..5 {
            let t = self.temperatures[i];
            self.surface(t, f, self.deep_ocean, f, f, 0.0, OCEANS[0][i]);
            self.surface(t, f, self.ocean, f, f, 0.0, OCEANS[1][i]);
        }
    }

    fn add_inland(&mut self) {
        self.add_mid_slice(span(-1.0, -0.93333334));
        self.add_high_slice(span(-0.93333334, -0.7666667));
        self.add_peaks(span(-0.7666667, -0.56666666));
        self.add_high_slice(span(-0.56666666, -0.4));
        self.add_mid_slice(span(-0.4, -0.26666668));
        self.add_low_slice(span(-0.26666668, -0.05));
        self.add_valleys(span(-0.05, 0.05));
        self.add_low_slice(span(0.05, 0.26666668));
        self.add_mid_slice(span(0.26666668, 0.4));
        self.add_high_slice(span(0.4, 0.56666666));
        self.add_peaks(span(0.56666666, 0.7666667));
        self.add_high_slice(span(0.7666667, 0.93333334));
        self.add_mid_slice(span(0.93333334, 1.0));
    }

    fn add_peaks(&mut self, w: Parameter) {
        let e = self.erosions;
        let (coast, near, mid, far) = (self.coast, self.near_inland, self.mid_inland, self.far_inland);
        for ti in 0..5 {
            let t = self.temperatures[ti];
            for hi in 0..5 {
                let h = self.humidities[hi];
                let middle = pick_middle(ti, hi, w);
                let middle_badlands = pick_middle_or_badlands_if_hot(ti, hi, w);
                let middle_badlands_slope = pick_middle_or_badlands_if_hot_or_slope_if_cold(ti, hi, w);
                let plateau = pick_plateau(ti, hi, w);
                let shattered = pick_shattered(ti, hi, w);
                let shattered_savanna = maybe_windswept_savanna(ti, hi, w, shattered);
                let peak = pick_peak(ti, hi, w);
                self.surface(t, h, join(coast, far), e[0], w, 0.0, peak);
                self.surface(t, h, join(coast, near), e[1], w, 0.0, middle_badlands_slope);
                self.surface(t, h, join(mid, far), e[1], w, 0.0, peak);
                self.surface(t, h, join(coast, near), join(e[2], e[3]), w, 0.0, middle);
                self.surface(t, h, join(mid, far), e[2], w, 0.0, plateau);
                self.surface(t, h, mid, e[3], w, 0.0, middle_badlands);
                self.surface(t, h, far, e[3], w, 0.0, plateau);
                self.surface(t, h, join(coast, far), e[4], w, 0.0, middle);
                self.surface(t, h, join(coast, near), e[5], w, 0.0, shattered_savanna);
                self.surface(t, h, join(mid, far), e[5], w, 0.0, shattered);
                self.surface(t, h, join(coast, far), e[6], w, 0.0, middle);
            }
        }
    }

    fn add_high_slice(&mut self, w: Parameter) {
        let e = self.erosions;
        let (coast, near, mid, far) = (self.coast, self.near_inland, self.mid_inland, self.far_inland);
        for ti in 0..5 {
            let t = self.temperatures[ti];
            for hi in 0..5 {
                let h = self.humidities[hi];
                let middle = pick_middle(ti, hi, w);
                let middle_badlands = pick_middle_or_badlands_if_hot(ti, hi, w);
                let middle_badlands_slope = pick_middle_or_badlands_if_hot_or_slope_if_cold(ti, hi, w);
                let plateau = pick_plateau(ti, hi, w);
                let shattered = pick_shattered(ti, hi, w);
                let middle_savanna = maybe_windswept_savanna(ti, hi, w, middle);
                let slope = pick_slope(ti, hi, w);
                let peak = pick_peak(ti, hi, w);
                self.surface(t, h, coast, join(e[0], e[1]), w, 0.0, middle);
                self.surface(t, h, near, e[0], w, 0.0, slope);
                self.surface(t, h, join(mid, far), e[0], w, 0.0, peak);
                self.surface(t, h, near, e[1], w, 0.0, middle_badlands_slope);
                self.surface(t, h, join(mid, far), e[1], w, 0.0, slope);
                self.surface(t, h, join(coast, near), join(e[2], e[3]), w, 0.0, middle);
                self.surface(t, h, join(mid, far), e[2], w, 0.0, plateau);
                self.surface(t, h, mid, e[3], w, 0.0, middle_badlands);
                self.surface(t, h, far, e[3], w, 0.0, plateau);
                self.surface(t, h, join(coast, far), e[4], w, 0.0, middle);
                self.surface(t, h, join(coast, near), e[5], w, 0.0, middle_savanna);
                self.surface(t, h, join(mid, far), e[5], w, 0.0, shattered);
                self.surface(t, h, join(coast, far), e[6], w, 0.0, middle);
            }
        }
    }

    fn add_swamps_and_stony_shore(&mut self, w: Parameter) {
        let e = self.erosions;
        let f = self.full;
        let tt = self.temperatures;
        self.surface(f, f, self.coast, join(e[0], e[2]), w, 0.0, STONY_SHORE);
        self.surface(join(tt[1], tt[2]), f, join(self.near_inland, self.far_inland), e[6], w, 0.0, SWAMP);
        self.surface(join(tt[3], tt[4]), f, join(self.near_inland, self.far_inland), e[6], w, 0.0, MANGROVE_SWAMP);
    }

    fn add_mid_slice(&mut self, w: Parameter) {
        self.add_swamps_and_stony_shore(w);
        let e = self.erosions;
        let (coast, near, mid, far) = (self.coast, self.near_inland, self.mid_inland, self.far_inland);
        for ti in 0..5 {
            let t = self.temperatures[ti];
            for hi in 0..5 {
                let h = self.humidities[hi];
                let middle = pick_middle(ti, hi, w);
                let middle_badlands = pick_middle_or_badlands_if_hot(ti, hi, w);
                let middle_badlands_slope = pick_middle_or_badlands_if_hot_or_slope_if_cold(ti, hi, w);
                let shattered = pick_shattered(ti, hi, w);
                let plateau = pick_plateau(ti, hi, w);
                let beach = pick_beach(ti, hi);
                let middle_savanna = maybe_windswept_savanna(ti, hi, w, middle);
                let shattered_coast = pick_shattered_coast(ti, hi, w);
                let slope = pick_slope(ti, hi, w);
                self.surface(t, h, join(near, far), e[0], w, 0.0, slope);
                self.surface(t, h, join(near, mid), e[1], w, 0.0, middle_badlands_slope);
                self.surface(t, h, far, e[1], w, 0.0, if ti == 0 { slope } else { plateau });
                self.surface(t, h, near, e[2], w, 0.0, middle);
                self.surface(t, h, mid, e[2], w, 0.0, middle_badlands);
                self.surface(t, h, far, e[2], w, 0.0, plateau);
                self.surface(t, h, join(coast, near), e[3], w, 0.0, middle);
                self.surface(t, h, join(mid, far), e[3], w, 0.0, middle_badlands);
                if w.max < 0 {
                    self.surface(t, h, coast, e[4], w, 0.0, beach);
                    self.surface(t, h, join(near, far), e[4], w, 0.0, middle);
                } else {
                    self.surface(t, h, join(coast, far), e[4], w, 0.0, middle);
                }
                self.surface(t, h, coast, e[5], w, 0.0, shattered_coast);
                self.surface(t, h, near, e[5], w, 0.0, middle_savanna);
                self.surface(t, h, join(mid, far), e[5], w, 0.0, shattered);
                if w.max < 0 {
                    self.surface(t, h, coast, e[6], w, 0.0, beach);
                } else {
                    self.surface(t, h, coast, e[6], w, 0.0, middle);
                }
                if ti == 0 {
                    self.surface(t, h, join(near, far), e[6], w, 0.0, middle);
                }
            }
        }
    }

    fn add_low_slice(&mut self, w: Parameter) {
        self.add_swamps_and_stony_shore(w);
        let e = self.erosions;
        let (coast, near, mid, far) = (self.coast, self.near_inland, self.mid_inland, self.far_inland);
        for ti in 0..5 {
            let t = self.temperatures[ti];
            for hi in 0..5 {
                let h = self.humidities[hi];
                let middle = pick_middle(ti, hi, w);
                let middle_badlands = pick_middle_or_badlands_if_hot(ti, hi, w);
                let middle_badlands_slope = pick_middle_or_badlands_if_hot_or_slope_if_cold(ti, hi, w);
                let beach = pick_beach(ti, hi);
                let middle_savanna = maybe_windswept_savanna(ti, hi, w, middle);
                let shattered_coast = pick_shattered_coast(ti, hi, w);
                self.surface(t, h, near, join(e[0], e[1]), w, 0.0, middle_badlands);
                self.surface(t, h, join(mid, far), join(e[0], e[1]), w, 0.0, middle_badlands_slope);
                self.surface(t, h, near, join(e[2], e[3]), w, 0.0, middle);
                self.surface(t, h, join(mid, far), join(e[2], e[3]), w, 0.0, middle_badlands);
                self.surface(t, h, coast, join(e[3], e[4]), w, 0.0, beach);
                self.surface(t, h, join(near, far), e[4], w, 0.0, middle);
                self.surface(t, h, coast, e[5], w, 0.0, shattered_coast);
                self.surface(t, h, near, e[5], w, 0.0, middle_savanna);
                self.surface(t, h, join(mid, far), e[5], w, 0.0, middle);
                self.surface(t, h, coast, e[6], w, 0.0, beach);
                if ti == 0 {
                    self.surface(t, h, join(near, far), e[6], w, 0.0, middle);
                }
            }
        }
    }

    fn add_valleys(&mut self, w: Parameter) {
        let e = self.erosions;
        let f = self.full;
        let tt = self.temperatures;
        let (fr, un) = (self.frozen, self.unfrozen);
        let (coast, near, far, inland) = (self.coast, self.near_inland, self.far_inland, self.inland);
        let neg = w.max < 0;
        self.surface(fr, f, coast, join(e[0], e[1]), w, 0.0, if neg { STONY_SHORE } else { FROZEN_RIVER });
        self.surface(un, f, coast, join(e[0], e[1]), w, 0.0, if neg { STONY_SHORE } else { RIVER });
        self.surface(fr, f, near, join(e[0], e[1]), w, 0.0, FROZEN_RIVER);
        self.surface(un, f, near, join(e[0], e[1]), w, 0.0, RIVER);
        self.surface(fr, f, join(coast, far), join(e[2], e[5]), w, 0.0, FROZEN_RIVER);
        self.surface(un, f, join(coast, far), join(e[2], e[5]), w, 0.0, RIVER);
        self.surface(fr, f, coast, e[6], w, 0.0, FROZEN_RIVER);
        self.surface(un, f, coast, e[6], w, 0.0, RIVER);
        self.surface(join(tt[1], tt[2]), f, join(inland, far), e[6], w, 0.0, SWAMP);
        self.surface(join(tt[3], tt[4]), f, join(inland, far), e[6], w, 0.0, MANGROVE_SWAMP);
        self.surface(fr, f, join(inland, far), e[6], w, 0.0, FROZEN_RIVER);
        for ti in 0..5 {
            let t = self.temperatures[ti];
            for hi in 0..5 {
                let h = self.humidities[hi];
                let b = pick_middle_or_badlands_if_hot(ti, hi, w);
                self.surface(t, h, join(self.mid_inland, far), join(e[0], e[1]), w, 0.0, b);
            }
        }
    }

    fn add_underground(&mut self) {
        let f = self.full;
        let e = self.erosions;
        self.underground(f, f, span(0.8, 1.0), f, f, 0.0, DRIPSTONE_CAVES);
        self.underground(f, span(0.7, 1.0), f, f, f, 0.0, LUSH_CAVES);
        self.bottom(f, f, f, join(e[0], e[1]), f, 0.0, DEEP_DARK);
    }
}

fn pick_middle(t: usize, h: usize, w: Parameter) -> &'static str {
    if w.max < 0 {
        MIDDLE_BIOMES[t][h]
    } else {
        MIDDLE_BIOMES_VARIANT[t][h].unwrap_or(MIDDLE_BIOMES[t][h])
    }
}

fn pick_middle_or_badlands_if_hot(t: usize, h: usize, w: Parameter) -> &'static str {
    if t == 4 {
        pick_badlands(h, w)
    } else {
        pick_middle(t, h, w)
    }
}

fn pick_middle_or_badlands_if_hot_or_slope_if_cold(t: usize, h: usize, w: Parameter) -> &'static str {
    if t == 0 {
        pick_slope(t, h, w)
    } else {
        pick_middle_or_badlands_if_hot(t, h, w)
    }
}

fn maybe_windswept_savanna(t: usize, h: usize, w: Parameter, otherwise: &'static str) -> &'static str {
    if t > 1 && h < 4 && w.max >= 0 {
        WINDSWEPT_SAVANNA
    } else {
        otherwise
    }
}

fn pick_shattered_coast(t: usize, h: usize, w: Parameter) -> &'static str {
    let b = if w.max >= 0 {
        pick_middle(t, h, w)
    } else {
        pick_beach(t, h)
    };
    maybe_windswept_savanna(t, h, w, b)
}

fn pick_beach(t: usize, _h: usize) -> &'static str {
    match t {
        0 => SNOWY_BEACH,
        4 => DESERT,
        _ => BEACH,
    }
}

fn pick_badlands(h: usize, w: Parameter) -> &'static str {
    if h < 2 {
        if w.max < 0 {
            BADLANDS
        } else {
            ERODED_BADLANDS
        }
    } else if h < 3 {
        BADLANDS
    } else {
        WOODED_BADLANDS
    }
}

fn pick_plateau(t: usize, h: usize, w: Parameter) -> &'static str {
    if w.max >= 0 {
        if let Some(b) = PLATEAU_BIOMES_VARIANT[t][h] {
            return b;
        }
    }
    PLATEAU_BIOMES[t][h]
}

fn pick_peak(t: usize, h: usize, w: Parameter) -> &'static str {
    if t <= 2 {
        if w.max < 0 {
            JAGGED_PEAKS
        } else {
            FROZEN_PEAKS
        }
    } else if t == 3 {
        STONY_PEAKS
    } else {
        pick_badlands(h, w)
    }
}

fn pick_slope(t: usize, h: usize, w: Parameter) -> &'static str {
    if t >= 3 {
        pick_plateau(t, h, w)
    } else if h <= 1 {
        SNOWY_SLOPES
    } else {
        GROVE
    }
}

fn pick_shattered(t: usize, h: usize, w: Parameter) -> &'static str {
    SHATTERED_BIOMES[t][h].unwrap_or_else(|| pick_middle(t, h, w))
}

/// The overworld's biome table, in the game's own row order.
pub fn overworld() -> Vec<ParameterPoint> {
    let mut b = Builder::new();
    b.add_biomes();
    b.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_the_expected_shape() {
        let t = overworld();
        // 1 mushroom + 10 ocean rows, each doubled for the two depth points,
        // plus the inland slices and three underground rows.
        assert!(t.len() > 7000, "{} rows", t.len());
        assert_eq!(t[0].biome, MUSHROOM_FIELDS);
        assert_eq!(t.last().unwrap().biome, DEEP_DARK);
        assert_eq!(t[0].space[2], Parameter { min: -12000, max: -10500 });
    }
}
