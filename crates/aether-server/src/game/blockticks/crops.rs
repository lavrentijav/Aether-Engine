//! Crop growth (C2): wheat, carrots, potatoes, beetroot, sugar cane, cactus.
//!
//! Transcribed from `CropBlock`, `BeetrootBlock`, `SugarCaneBlock` and
//! `CactusBlock` in the 1.21.11 server rather than from memory. Crops, sugar
//! cane and cactus advance on **random ticks** — vanilla visits three random
//! blocks per 16³ section per tick, and this engine does the same through
//! [`crate::session::DemoWorld`]'s sampler.
//!
//! Two deviations are deliberate, and both are noted in `KNOWN_ISSUES` A.11:
//!
//! * **Light is full-bright.** Vanilla's `CropBlock.randomTick` only grows
//!   when `getRawBrightness(pos, 0) >= 9`, and `canSurvive` wants `>= 8`. This
//!   engine does not simulate light, so every random tick is treated as bright
//!   enough. Crops therefore grow in the dark, and under a roof, unlike
//!   vanilla's.
//! * **The `DIRT` and `SAND` block tags are name lists here.** The engine has
//!   no block-tag table, so the families sugar cane and cactus accept are
//!   spelled out below.

use aether_world::registry::blocks;
use aether_world::{BlockProperties, BlockStateId};

use super::{is_named, name_of, short, Cells, Pos};
use crate::game::tables::Rng;

/// Vanilla's `nextInt((int)(25.0F / f) + 1)` truncates, so this is `f32`.
const GROWTH_BASE: f32 = 25.0;

/// The blocks this module grows, by name.
pub(super) fn is_crop(name: &str) -> bool {
    matches!(short(name), "wheat" | "carrots" | "potatoes" | "beetroots")
}

pub(super) fn is_sugar_cane(name: &str) -> bool {
    short(name) == "sugar_cane"
}

pub(super) fn is_cactus(name: &str) -> bool {
    short(name) == "cactus"
}

/// The maximum `age` of a crop, `None` for a block that is not one.
pub(super) fn max_age(name: &str) -> Option<i32> {
    match short(name) {
        "wheat" | "carrots" | "potatoes" => Some(7),
        "beetroots" => Some(3),
        "sugar_cane" | "cactus" => Some(15),
        _ => None,
    }
}

/// The `DIRT` tag, spelled out — see the module note.
fn is_dirt(name: &str) -> bool {
    matches!(
        short(name),
        "dirt"
            | "grass_block"
            | "podzol"
            | "coarse_dirt"
            | "mycelium"
            | "rooted_dirt"
            | "moss_block"
            | "mud"
            | "muddy_mangrove_roots"
    )
}

/// The `SAND` tag, spelled out — see the module note.
fn is_sand(name: &str) -> bool {
    matches!(short(name), "sand" | "red_sand" | "suspicious_sand")
}

fn is_lava(state: BlockStateId) -> bool {
    is_named(state, "minecraft:lava")
}

fn is_solid(state: BlockStateId) -> bool {
    blocks::props_of_state(state).is_some_and(|p: BlockProperties| p.solid)
}

/// Vanilla's `CropBlock.getGrowthSpeed`, for a crop whose block name is
/// `crop`.
///
/// A moist farmland block in the 3×3 below contributes three, a dry one one,
/// a diagonal is divided by four, and a crop crowded by the same crop north
/// or south *and* east or west — or on a diagonal — grows at half speed.
pub(super) fn growth_speed(get: &Cells<'_>, pos: Pos, crop: &str) -> f32 {
    let (x, y, z) = pos;
    let mut f = 1.0f32;
    for dx in -1..=1 {
        for dz in -1..=1 {
            let mut g = 0.0f32;
            let below = get(x + dx, y - 1, z + dz);
            if is_named(below, "minecraft:farmland") {
                g = 1.0;
                let moisture = super::prop(below, "moisture")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(0);
                if moisture > 0 {
                    g = 3.0;
                }
            }
            if dx != 0 || dz != 0 {
                g /= 4.0;
            }
            f += g;
        }
    }
    let same = |dx: i32, dz: i32| name_of(get(x + dx, y, z + dz)).is_some_and(|n| n == crop);
    let we = same(-1, 0) || same(1, 0);
    let ns = same(0, -1) || same(0, 1);
    // Vanilla halves once for a crop on both axes, or once for a diagonal
    // neighbour; either way the same division.
    if (we && ns) || same(-1, -1) || same(1, -1) || same(1, 1) || same(-1, 1) {
        f /= 2.0;
    }
    f
}

/// One random tick of a `CropBlock` (wheat, carrots, potatoes).
///
/// Returns the state to put the crop in, if it grew.
pub(super) fn grow_crop(
    get: &Cells<'_>,
    rng: &mut Rng,
    pos: Pos,
    state: BlockStateId,
    name: &str,
) -> Option<BlockStateId> {
    // Beetroot rolls `nextInt(3) != 0` *before* the common crop tick: one in
    // three random ticks does nothing at all.
    if short(name) == "beetroots" && rng.range(0, 2) == 0 {
        return None;
    }
    let max = max_age(name)?;
    let age = super::prop(state, "age")?.parse::<i32>().ok()?;
    if age >= max {
        return None;
    }
    // Full-bright: vanilla's `getRawBrightness(pos, 0) >= 9` is assumed true.
    let f = growth_speed(get, pos, name);
    let bound = (GROWTH_BASE / f) as i32 + 1;
    if rng.range(0, bound - 1) != 0 {
        return None;
    }
    super::with_prop(state, "age", &(age + 1).to_string())
}

/// One random tick of a sugar cane block: lengthen the column towards three.
pub(super) fn grow_sugar_cane(
    get: &Cells<'_>,
    pos: Pos,
    state: BlockStateId,
) -> Option<Vec<(Pos, BlockStateId)>> {
    let (x, y, z) = pos;
    if !super::is_air(get(x, y + 1, z)) {
        return None;
    }
    let mut i = 1;
    while is_named(get(x, y - i, z), "minecraft:sugar_cane") {
        i += 1;
    }
    if i >= 3 {
        return None;
    }
    let age = super::prop(state, "age")?.parse::<i32>().ok()?;
    if age == 15 {
        let fresh = blocks::default_state("minecraft:sugar_cane")?;
        Some(vec![
            ((x, y + 1, z), fresh),
            (pos, super::with_prop(state, "age", "0")?),
        ])
    } else {
        Some(vec![(
            pos,
            super::with_prop(state, "age", &(age + 1).to_string())?,
        )])
    }
}

/// One random tick of a cactus block: age to fifteen, occasionally flowering
/// at eight, then a new cactus above if the column is short enough.
pub(super) fn grow_cactus(
    get: &Cells<'_>,
    rng: &mut Rng,
    pos: Pos,
    state: BlockStateId,
) -> Option<Vec<(Pos, BlockStateId)>> {
    let (x, y, z) = pos;
    let above = (x, y + 1, z);
    if !super::is_air(get(x, y + 1, z)) {
        return None;
    }
    let age = super::prop(state, "age")?.parse::<i32>().ok()?;
    let mut i = 1;
    while is_named(get(x, y - i, z), "minecraft:cactus") {
        i += 1;
        if i == 3 && age == 15 {
            return None;
        }
    }
    let mut changes = Vec::new();
    if age == 8 && can_survive_cactus(get, above) {
        let chance = if i >= 3 { 0.25 } else { 0.1 };
        if rng.f64() <= chance {
            changes.push((above, blocks::default_state("minecraft:cactus_flower")?));
        }
    } else if age == 15 && i < 3 {
        changes.push((above, blocks::default_state("minecraft:cactus")?));
        changes.push((pos, super::with_prop(state, "age", "0")?));
    }
    if age < 15 {
        changes.push((pos, super::with_prop(state, "age", &(age + 1).to_string())?));
    }
    Some(changes)
}

/// `SugarCaneBlock.canSurvive`: on sugar cane, or on dirt/sand with water (or
/// frosted ice) touching that support.
pub(super) fn can_survive_sugar_cane(get: &Cells<'_>, pos: Pos) -> bool {
    let (x, y, z) = pos;
    let below = get(x, y - 1, z);
    if is_named(below, "minecraft:sugar_cane") {
        return true;
    }
    let Some(name) = name_of(below) else {
        return false;
    };
    if !is_dirt(name) && !is_sand(name) {
        return false;
    }
    let (bx, bz) = (x, z);
    [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dz)| {
        let side = get(bx + dx, y - 1, bz + dz);
        super::is_water(side) || is_named(side, "minecraft:frosted_ice")
    })
}

/// `CactusBlock.canSurvive`: nothing solid or lava to a side, and cactus or
/// sand below, and not a liquid above.
pub(super) fn can_survive_cactus(get: &Cells<'_>, pos: Pos) -> bool {
    let (x, y, z) = pos;
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let side = get(x + dx, y, z + dz);
        if is_solid(side) || is_lava(side) {
            return false;
        }
    }
    let below = get(x, y - 1, z);
    let Some(name) = name_of(below) else {
        return false;
    };
    (is_named(below, "minecraft:cactus") || is_sand(name)) && !super::is_liquid(get(x, y + 1, z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_api::block_ids;
    use std::collections::HashMap;

    struct Map(HashMap<Pos, BlockStateId>);

    impl Map {
        fn new() -> Self {
            Self(HashMap::new())
        }
        fn set(mut self, pos: Pos, state: BlockStateId) -> Self {
            self.0.insert(pos, state);
            self
        }
        fn put(self, pos: Pos, name: &str) -> Self {
            self.set(pos, blocks::default_state(name).unwrap())
        }
        fn get(&self) -> impl Fn(i32, i32, i32) -> BlockStateId + '_ {
            move |x, y, z| self.0.get(&(x, y, z)).copied().unwrap_or(block_ids::AIR)
        }
    }

    fn with(state: BlockStateId, key: &str, value: &str) -> BlockStateId {
        super::super::with_prop(state, key, value).unwrap()
    }

    fn crop(name: &str, age: i32) -> BlockStateId {
        with(
            blocks::default_state(name).unwrap(),
            "age",
            &age.to_string(),
        )
    }

    fn farmland(moisture: u32) -> BlockStateId {
        with(
            blocks::default_state("minecraft:farmland").unwrap(),
            "moisture",
            &moisture.to_string(),
        )
    }

    #[test]
    fn moist_farmland_grows_faster_than_dry() {
        // Base 1, plus the centre farmland: 1 dry, 3 moist.
        let dry = Map::new().set((0, 0, 0), farmland(0));
        let moist = Map::new().set((0, 0, 0), farmland(7));
        assert_eq!(growth_speed(&dry.get(), (0, 1, 0), "minecraft:wheat"), 2.0);
        assert_eq!(
            growth_speed(&moist.get(), (0, 1, 0), "minecraft:wheat"),
            4.0
        );
    }

    #[test]
    fn farmland_in_the_ring_adds_a_quarter() {
        // Base 1 + centre 1 + orthogonal 1/4 = 2.25
        let m = Map::new()
            .set((0, 0, 0), farmland(0))
            .set((1, 0, 0), farmland(0));
        assert_eq!(growth_speed(&m.get(), (0, 1, 0), "minecraft:wheat"), 2.25);
    }

    #[test]
    fn a_crowded_crop_grows_at_half_speed() {
        let m = Map::new()
            .set((0, 0, 0), farmland(0))
            .set((1, 1, 0), crop("minecraft:wheat", 2))
            .set((0, 1, 1), crop("minecraft:wheat", 2));
        // east (we) and south (ns) both crop -> base 2 is halved.
        assert_eq!(growth_speed(&m.get(), (0, 1, 0), "minecraft:wheat"), 1.0);
    }

    #[test]
    fn beetroot_mature_at_three() {
        assert_eq!(max_age("minecraft:beetroots"), Some(3));
        assert_eq!(max_age("minecraft:wheat"), Some(7));
        assert_eq!(max_age("minecraft:carrots"), Some(7));
        assert_eq!(max_age("minecraft:sugar_cane"), Some(15));
        assert_eq!(max_age("minecraft:stone"), None);
    }

    #[test]
    fn a_crop_grows_to_maturity_over_random_ticks() {
        let m = Map::new().set((0, 0, 0), farmland(7));
        let get = m.get();
        let mut rng = Rng::new(0x1234_5678);
        let mut state = crop("minecraft:wheat", 0);
        for _ in 0..100_000 {
            if let Some(next) = grow_crop(&get, &mut rng, (0, 1, 0), state, "minecraft:wheat") {
                state = next;
            }
            if super::super::prop(state, "age") == Some("7") {
                break;
            }
        }
        assert_eq!(super::super::prop(state, "age"), Some("7"));
    }

    #[test]
    fn sugar_cane_stops_once_the_column_is_three() {
        // Column of three: the top block may not grow a fourth.
        let three = Map::new()
            .put((0, 0, 0), "minecraft:sand")
            .put((1, 0, 0), "minecraft:water")
            .set((0, 1, 0), crop("minecraft:sugar_cane", 15))
            .set((0, 2, 0), crop("minecraft:sugar_cane", 15))
            .set((0, 3, 0), crop("minecraft:sugar_cane", 15));
        let get = three.get();
        let top = three.0[&(0, 3, 0)];
        assert!(grow_sugar_cane(&get, (0, 3, 0), top).is_none());

        // Column of two: the top may grow a third.
        let two = Map::new()
            .put((0, 0, 0), "minecraft:sand")
            .put((1, 0, 0), "minecraft:water")
            .set((0, 1, 0), crop("minecraft:sugar_cane", 15))
            .set((0, 2, 0), crop("minecraft:sugar_cane", 15));
        let grow = grow_sugar_cane(&two.get(), (0, 2, 0), two.0[&(0, 2, 0)]);
        let changes = grow.expect("a third stalk");
        assert!(changes.iter().any(|(p, _)| *p == (0, 3, 0)));
    }

    #[test]
    fn sugar_cane_needs_water_beside_its_support() {
        let dry = Map::new().put((0, 0, 0), "minecraft:sand");
        assert!(!can_survive_sugar_cane(&dry.get(), (0, 1, 0)));
        let wet = Map::new()
            .put((0, 0, 0), "minecraft:sand")
            .put((1, 0, 0), "minecraft:water");
        assert!(can_survive_sugar_cane(&wet.get(), (0, 1, 0)));
    }

    #[test]
    fn cactus_refuses_a_solid_neighbour() {
        let m = Map::new().put((0, 0, 0), "minecraft:sand");
        assert!(can_survive_cactus(&m.get(), (0, 1, 0)));
        let blocked = Map::new()
            .put((0, 0, 0), "minecraft:sand")
            .put((1, 1, 0), "minecraft:stone");
        assert!(!can_survive_cactus(&blocked.get(), (0, 1, 0)));
    }
}
