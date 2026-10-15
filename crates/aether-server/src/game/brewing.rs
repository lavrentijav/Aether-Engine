//! Brewing and potions.
//!
//! A potion is a name in the `minecraft:potion` registry — `minecraft:water`,
//! `minecraft:awkward`, `minecraft:long_swiftness` — and its effects are a
//! pure function of that name. That is exactly how vanilla models it
//! (`net.minecraft.world.item.alchemy.Potion`): the registry entry carries the
//! `MobEffectInstance` list, and the item only points at it through the
//! `minecraft:potion_contents` component. This module is the engine's copy of
//! that registry plus the brewing table, transcribed from the 1.21.11 server's
//! `Potions` and `PotionBrewing` rather than from memory.
//!
//! The registry index of each potion is its position in [`POTIONS`], which is
//! the order `Potions` registers them in, and it is what travels on the wire.
//! Change the order and every stored bottle would point at the wrong potion,
//! so the two must stay in step.

use super::effects::{
    self, Effect, FIRE_RESISTANCE, INFESTED, INSTANT_DAMAGE, INSTANT_HEALTH, INVISIBILITY,
    JUMP_BOOST, LUCK, NIGHT_VISION, OOZING, POISON, REGENERATION, RESISTANCE, SLOWNESS,
    SLOW_FALLING, SPEED, STRENGTH, WATER_BREATHING, WEAKNESS, WEAVING, WIND_CHARGED,
};

/// One entry of the `minecraft:potion` registry: its name and the effects it
/// applies, each as `(effect id, amplifier, duration in ticks)`.
pub struct PotionDef {
    pub name: &'static str,
    pub effects: &'static [(u8, u8, i32)],
}

/// The whole registry, in registration order. The index is the wire id.
pub static POTIONS: &[PotionDef] = &[
    def("water", &[]),
    def("mundane", &[]),
    def("thick", &[]),
    def("awkward", &[]),
    def("night_vision", &[(NIGHT_VISION, 0, 3600)]),
    def("long_night_vision", &[(NIGHT_VISION, 0, 9600)]),
    def("invisibility", &[(INVISIBILITY, 0, 3600)]),
    def("long_invisibility", &[(INVISIBILITY, 0, 9600)]),
    def("leaping", &[(JUMP_BOOST, 0, 3600)]),
    def("long_leaping", &[(JUMP_BOOST, 0, 9600)]),
    def("strong_leaping", &[(JUMP_BOOST, 1, 1800)]),
    def("fire_resistance", &[(FIRE_RESISTANCE, 0, 3600)]),
    def("long_fire_resistance", &[(FIRE_RESISTANCE, 0, 9600)]),
    def("swiftness", &[(SPEED, 0, 3600)]),
    def("long_swiftness", &[(SPEED, 0, 9600)]),
    def("strong_swiftness", &[(SPEED, 1, 1800)]),
    def("slowness", &[(SLOWNESS, 0, 1800)]),
    def("long_slowness", &[(SLOWNESS, 0, 4800)]),
    def("strong_slowness", &[(SLOWNESS, 3, 400)]),
    def("turtle_master", &[(SLOWNESS, 3, 400), (RESISTANCE, 2, 400)]),
    def(
        "long_turtle_master",
        &[(SLOWNESS, 3, 800), (RESISTANCE, 2, 800)],
    ),
    def(
        "strong_turtle_master",
        &[(SLOWNESS, 5, 400), (RESISTANCE, 3, 400)],
    ),
    def("water_breathing", &[(WATER_BREATHING, 0, 3600)]),
    def("long_water_breathing", &[(WATER_BREATHING, 0, 9600)]),
    def("healing", &[(INSTANT_HEALTH, 0, 1)]),
    def("strong_healing", &[(INSTANT_HEALTH, 1, 1)]),
    def("harming", &[(INSTANT_DAMAGE, 0, 1)]),
    def("strong_harming", &[(INSTANT_DAMAGE, 1, 1)]),
    def("poison", &[(POISON, 0, 900)]),
    def("long_poison", &[(POISON, 0, 1800)]),
    def("strong_poison", &[(POISON, 1, 432)]),
    def("regeneration", &[(REGENERATION, 0, 900)]),
    def("long_regeneration", &[(REGENERATION, 0, 1800)]),
    def("strong_regeneration", &[(REGENERATION, 1, 450)]),
    def("strength", &[(STRENGTH, 0, 3600)]),
    def("long_strength", &[(STRENGTH, 0, 9600)]),
    def("strong_strength", &[(STRENGTH, 1, 1800)]),
    def("weakness", &[(WEAKNESS, 0, 1800)]),
    def("long_weakness", &[(WEAKNESS, 0, 4800)]),
    def("luck", &[(LUCK, 0, 6000)]),
    def("slow_falling", &[(SLOW_FALLING, 0, 1800)]),
    def("long_slow_falling", &[(SLOW_FALLING, 0, 4800)]),
    def("wind_charged", &[(WIND_CHARGED, 0, 3600)]),
    def("weaving", &[(WEAVING, 0, 3600)]),
    def("oozing", &[(OOZING, 0, 3600)]),
    def("infested", &[(INFESTED, 0, 3600)]),
];

const fn def(name: &'static str, effects: &'static [(u8, u8, i32)]) -> PotionDef {
    PotionDef { name, effects }
}

/// The wire id of `minecraft:water`, the potion a fresh bottle holds.
pub const WATER: u16 = 0;

/// The registry index of `name`, if this build knows it.
pub fn potion_id(name: &str) -> Option<u16> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    POTIONS
        .iter()
        .position(|p| p.name == short)
        .map(|i| i as u16)
}

/// The definition of registry entry `id`.
pub fn def_of(id: u16) -> Option<&'static PotionDef> {
    POTIONS.get(id as usize)
}

/// The effects a potion applies when drunk, at full strength. Instant effects
/// come back too; the caller applies them at once (see [`effects::is_instant`]).
pub fn effects_of(id: u16) -> Vec<Effect> {
    effects_of_scaled(id, 1.0)
}

/// The effects of `id` scaled by `factor`, the splash falloff
/// (`1 - distance / 4`, clamped to `0..=1`). A non-instant effect whose
/// scaled duration would round to 20 ticks or less is dropped, exactly as
/// vanilla's `ThrownSplashPotion` does with `endsWithin(20)`.
pub fn effects_of_scaled(id: u16, factor: f64) -> Vec<Effect> {
    let Some(def) = def_of(id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &(effect, amplifier, duration) in def.effects {
        let scaled = (factor * duration as f64 + 0.5) as i32;
        if !effects::is_instant(effect) && scaled <= 20 {
            continue;
        }
        out.push(Effect::new(effect, amplifier, scaled.max(1)));
    }
    out
}

/// `Items.LINGERING_POTION`'s `POTION_DURATION_SCALE`: a lingering potion's
/// cloud applies its effects at a quarter of the registry duration.
pub const LINGERING_DURATION_SCALE: f64 = 0.25;

/// The effects a lingering potion's cloud applies, from the registry entry at
/// the lingering `POTION_DURATION_SCALE`. Vanilla's `AreaEffectCloud` builds
/// the list with `forEachEffect(consumer, potionDurationScale)`, which floors
/// the scaled duration and never drops a short one (unlike a splash).
pub fn effects_of_lingering(id: u16) -> Vec<Effect> {
    let Some(def) = def_of(id) else {
        return Vec::new();
    };
    def.effects
        .iter()
        .map(|&(effect, amplifier, duration)| {
            let scaled = if effects::is_instant(effect) {
                duration
            } else {
                ((duration as f64 * LINGERING_DURATION_SCALE).floor() as i32).max(1)
            };
            Effect::new(effect, amplifier, scaled)
        })
        .collect()
}

/// Whether any of the potion's effects is instant — vanilla's
/// `Potion.hasInstantEffects`, which chooses the 2007 (instant) over the 2002
/// (splash) level event.
pub fn has_instant_effects(id: u16) -> bool {
    def_of(id)
        .map(|d| d.effects.iter().any(|&(e, _, _)| effects::is_instant(e)))
        .unwrap_or(false)
}

/// The colour a potion's particles and overlay use, vanilla's
/// `PotionContents.getColorOptional`: the amplifier-weighted average of each
/// effect's own colour, or `BASE_POTION_COLOR` when the potion has none.
pub fn potion_color(id: u16) -> i32 {
    let mut r = 0i32;
    let mut g = 0i32;
    let mut b = 0i32;
    let mut total = 0i32;
    for e in effects_of(id) {
        let c = effect_color(e.id);
        let n = e.amplifier as i32 + 1;
        r += n * ((c >> 16) & 0xFF);
        g += n * ((c >> 8) & 0xFF);
        b += n * (c & 0xFF);
        total += n;
    }
    if total == 0 {
        BASE_POTION_COLOR
    } else {
        argb(r / total, g / total, b / total)
    }
}

/// `PotionContents.BASE_POTION_COLOR`, the water potion's colour.
pub const BASE_POTION_COLOR: i32 = -13083194;

fn argb(r: i32, g: i32, b: i32) -> i32 {
    (0xFF << 24) | (r << 16) | (g << 8) | b
}

/// One effect's colour, from `MobEffects`' own registration.
fn effect_color(id: u8) -> i32 {
    use effects::*;
    match id {
        SPEED => 3402751,
        SLOWNESS => 9154528,
        STRENGTH => 16762624,
        INSTANT_HEALTH => 16262179,
        INSTANT_DAMAGE => 11101546,
        JUMP_BOOST => 16646020,
        NAUSEA => 5578058,
        REGENERATION => 13458603,
        RESISTANCE => 9520880,
        FIRE_RESISTANCE => 16750848,
        WATER_BREATHING => 10017472,
        INVISIBILITY => 16185078,
        BLINDNESS => 2039587,
        NIGHT_VISION => 12779366,
        HUNGER => 5797459,
        WEAKNESS => 4738376,
        POISON => 8889187,
        WITHER => 7561558,
        ABSORPTION => 2445989,
        SATURATION => 16262179,
        LUCK => 5882118,
        SLOW_FALLING => 15978425,
        WIND_CHARGED => 12438015,
        WEAVING => 7891290,
        OOZING => 10092451,
        INFESTED => 9214860,
        _ => 0x00FF_FFFF,
    }
}

/// Whether `item` is one of the three drinkable/thrown potion items.
pub fn is_potion_item(item: &str) -> bool {
    matches!(short(item), "potion" | "splash_potion" | "lingering_potion")
}

/// Whether `item` is the drinkable bottle (not splash or lingering).
pub fn is_drinkable(item: &str) -> bool {
    short(item) == "potion"
}

fn short(item: &str) -> &str {
    item.strip_prefix("minecraft:").unwrap_or(item)
}

// ---------------------------------------------------------------------------
// The brewing table.
// ---------------------------------------------------------------------------

/// One potion-to-potion mix: brewing `ingredient` into `from` yields `to`.
struct Mix {
    from: &'static str,
    ingredient: &'static str,
    to: &'static str,
}

/// One item-to-item container mix: brewing `ingredient` into `from` yields
/// `to`, keeping the potion unchanged.
struct ContainerMix {
    from: &'static str,
    ingredient: &'static str,
    to: &'static str,
}

/// `PotionBrewing.addVanillaMixes`, in order. An `addStartMix(item, to)` is
/// two entries — `water + item -> mundane` and `awkward + item -> to`.
static MIXES: &[Mix] = &[
    pmix("water", "glowstone_dust", "thick"),
    pmix("water", "redstone", "mundane"),
    pmix("water", "nether_wart", "awkward"),
    pmix("water", "breeze_rod", "mundane"),
    pmix("awkward", "breeze_rod", "wind_charged"),
    pmix("water", "slime_block", "mundane"),
    pmix("awkward", "slime_block", "oozing"),
    pmix("water", "stone", "mundane"),
    pmix("awkward", "stone", "infested"),
    pmix("water", "cobweb", "mundane"),
    pmix("awkward", "cobweb", "weaving"),
    pmix("awkward", "golden_carrot", "night_vision"),
    pmix("night_vision", "redstone", "long_night_vision"),
    pmix("night_vision", "fermented_spider_eye", "invisibility"),
    pmix(
        "long_night_vision",
        "fermented_spider_eye",
        "long_invisibility",
    ),
    pmix("invisibility", "redstone", "long_invisibility"),
    pmix("water", "magma_cream", "mundane"),
    pmix("awkward", "magma_cream", "fire_resistance"),
    pmix("fire_resistance", "redstone", "long_fire_resistance"),
    pmix("water", "rabbit_foot", "mundane"),
    pmix("awkward", "rabbit_foot", "leaping"),
    pmix("leaping", "redstone", "long_leaping"),
    pmix("leaping", "glowstone_dust", "strong_leaping"),
    pmix("leaping", "fermented_spider_eye", "slowness"),
    pmix("long_leaping", "fermented_spider_eye", "long_slowness"),
    pmix("slowness", "redstone", "long_slowness"),
    pmix("slowness", "glowstone_dust", "strong_slowness"),
    pmix("awkward", "turtle_helmet", "turtle_master"),
    pmix("turtle_master", "redstone", "long_turtle_master"),
    pmix("turtle_master", "glowstone_dust", "strong_turtle_master"),
    pmix("swiftness", "fermented_spider_eye", "slowness"),
    pmix("long_swiftness", "fermented_spider_eye", "long_slowness"),
    pmix("water", "sugar", "mundane"),
    pmix("awkward", "sugar", "swiftness"),
    pmix("swiftness", "redstone", "long_swiftness"),
    pmix("swiftness", "glowstone_dust", "strong_swiftness"),
    pmix("awkward", "pufferfish", "water_breathing"),
    pmix("water_breathing", "redstone", "long_water_breathing"),
    pmix("water", "glistering_melon_slice", "mundane"),
    pmix("awkward", "glistering_melon_slice", "healing"),
    pmix("healing", "glowstone_dust", "strong_healing"),
    pmix("healing", "fermented_spider_eye", "harming"),
    pmix("strong_healing", "fermented_spider_eye", "strong_harming"),
    pmix("harming", "glowstone_dust", "strong_harming"),
    pmix("poison", "fermented_spider_eye", "harming"),
    pmix("long_poison", "fermented_spider_eye", "harming"),
    pmix("strong_poison", "fermented_spider_eye", "strong_harming"),
    pmix("water", "spider_eye", "mundane"),
    pmix("awkward", "spider_eye", "poison"),
    pmix("poison", "redstone", "long_poison"),
    pmix("poison", "glowstone_dust", "strong_poison"),
    pmix("water", "ghast_tear", "mundane"),
    pmix("awkward", "ghast_tear", "regeneration"),
    pmix("regeneration", "redstone", "long_regeneration"),
    pmix("regeneration", "glowstone_dust", "strong_regeneration"),
    pmix("water", "blaze_powder", "mundane"),
    pmix("awkward", "blaze_powder", "strength"),
    pmix("strength", "redstone", "long_strength"),
    pmix("strength", "glowstone_dust", "strong_strength"),
    pmix("water", "fermented_spider_eye", "weakness"),
    pmix("weakness", "redstone", "long_weakness"),
    pmix("awkward", "phantom_membrane", "slow_falling"),
    pmix("slow_falling", "redstone", "long_slow_falling"),
];

const fn pmix(from: &'static str, ingredient: &'static str, to: &'static str) -> Mix {
    Mix {
        from,
        ingredient,
        to,
    }
}

static CONTAINER_MIXES: &[ContainerMix] = &[
    cmix("potion", "gunpowder", "splash_potion"),
    cmix("splash_potion", "dragon_breath", "lingering_potion"),
];

const fn cmix(from: &'static str, ingredient: &'static str, to: &'static str) -> ContainerMix {
    ContainerMix {
        from,
        ingredient,
        to,
    }
}

/// Whether `ingredient` (an item name) is an ingredient any brew accepts —
/// vanilla's `PotionBrewing.isIngredient`, which the brewing stand's ingredient
/// slot uses to decide what may go in.
pub fn is_ingredient(ingredient: &str) -> bool {
    let s = short(ingredient);
    MIXES.iter().any(|m| m.ingredient == s) || CONTAINER_MIXES.iter().any(|m| m.ingredient == s)
}

/// Whether `ingredient` is a valid *brewing stand fuel* — the
/// `minecraft:brewing_fuel` item tag, which in vanilla holds blaze powder
/// alone.
pub fn is_fuel(ingredient: &str) -> bool {
    short(ingredient) == "blaze_powder"
}

/// Whether the brewing stand can accept `item` in one of its three bottle
/// slots: a potion, a splash or a lingering potion, or a glass bottle.
pub fn is_bottle_item(item: &str) -> bool {
    matches!(
        short(item),
        "potion" | "splash_potion" | "lingering_potion" | "glass_bottle"
    )
}

/// The result of brewing `ingredient` into the stack whose item is
/// `input_item` and whose potion is `input_potion`. `Some((item, potion))`
/// when a recipe matches; `None` when it does not. `potion` is the registry
/// index, or `None` for a container mix that keeps an item's own contents.
pub fn mix(
    input_item: &str,
    input_potion: Option<u16>,
    ingredient: &str,
) -> Option<(String, Option<u16>)> {
    let item = short(input_item);
    let ing = short(ingredient);
    // A container mix changes the item and keeps the potion.
    for m in CONTAINER_MIXES {
        if m.from == item && m.ingredient == ing {
            return Some((format!("minecraft:{}", m.to), input_potion));
        }
    }
    // A potion mix changes the potion and keeps the item.
    let from = def_of(input_potion?)?;
    for m in MIXES {
        if m.from == from.name && m.ingredient == ing {
            return Some((
                input_item.to_owned(),
                Some(potion_id(m.to).expect("every mix target is registered")),
            ));
        }
    }
    None
}

/// Whether brewing `ingredient` into this stack would change anything — the
/// condition that lets a brewing stand start, vanilla's `hasMix`.
pub fn has_mix(input_item: &str, input_potion: Option<u16>, ingredient: &str) -> bool {
    mix(input_item, input_potion, ingredient).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> u16 {
        potion_id(name).unwrap_or_else(|| panic!("{name} is a potion"))
    }

    #[test]
    fn the_registry_matches_vanillas_registration_order() {
        assert_eq!(id("minecraft:water"), 0);
        assert_eq!(id("minecraft:awkward"), 3);
        assert_eq!(id("minecraft:swiftness"), 13);
        assert_eq!(id("minecraft:long_swiftness"), 14);
        assert_eq!(id("minecraft:strong_swiftness"), 15);
        assert_eq!(id("minecraft:healing"), 24);
        assert_eq!(id("minecraft:harming"), 26);
        assert_eq!(id("minecraft:infested"), 45);
        assert_eq!(POTIONS.len(), 46);
        assert_eq!(POTIONS[0].name, "water");
    }

    #[test]
    fn water_and_thick_and_mundane_have_no_effects() {
        for name in ["water", "thick", "mundane", "awkward"] {
            assert!(effects_of(id(name)).is_empty(), "{name}");
        }
        assert_eq!(effects_of(id("swiftness")).len(), 1);
        assert_eq!(effects_of(id("turtle_master")).len(), 2);
    }

    #[test]
    fn each_potion_effect_matches_the_registry_entry() {
        let speed = effects_of(id("swiftness"))[0];
        assert_eq!(
            (speed.id, speed.amplifier, speed.duration),
            (SPEED, 0, 3600)
        );
        let strong = effects_of(id("strong_swiftness"))[0];
        assert_eq!(
            (strong.id, strong.amplifier, strong.duration),
            (SPEED, 1, 1800)
        );
        let strong_slow = effects_of(id("strong_slowness"))[0];
        assert_eq!((strong_slow.amplifier, strong_slow.duration), (3, 400));
        let arrow = effects_of(id("harming"))[0];
        assert_eq!((arrow.id, arrow.duration), (INSTANT_DAMAGE, 1));
    }

    #[test]
    fn the_base_families_brew() {
        // Water and nether wart make awkward; glowstone and redstone make
        // thick and mundane.
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("water")),
                "minecraft:nether_wart"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("awkward"))))
        );
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("water")),
                "minecraft:glowstone_dust"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("thick"))))
        );
        assert_eq!(
            mix("minecraft:potion", Some(id("water")), "minecraft:redstone"),
            Some(("minecraft:potion".to_owned(), Some(id("mundane"))))
        );
    }

    #[test]
    fn awkward_plus_a_start_ingredient_makes_the_effect_potion() {
        // Every `addStartMix` item: on awkward it makes the effect potion, and
        // on water it makes mundane.
        let start_mixes = [
            ("minecraft:magma_cream", "fire_resistance"),
            ("minecraft:rabbit_foot", "leaping"),
            ("minecraft:sugar", "swiftness"),
            ("minecraft:glistering_melon_slice", "healing"),
            ("minecraft:spider_eye", "poison"),
            ("minecraft:ghast_tear", "regeneration"),
            ("minecraft:blaze_powder", "strength"),
        ];
        for (ingredient, potion) in start_mixes {
            assert_eq!(
                mix("minecraft:potion", Some(id("awkward")), ingredient),
                Some(("minecraft:potion".to_owned(), Some(id(potion)))),
                "{ingredient} -> {potion}"
            );
            assert_eq!(
                mix("minecraft:potion", Some(id("water")), ingredient),
                Some(("minecraft:potion".to_owned(), Some(id("mundane")))),
                "{ingredient} on water"
            );
        }
        // Explicit `addMix` items: awkward makes the effect potion, but water
        // has no rule for them.
        let explicit = [
            ("minecraft:golden_carrot", "night_vision"),
            ("minecraft:pufferfish", "water_breathing"),
            ("minecraft:phantom_membrane", "slow_falling"),
        ];
        for (ingredient, potion) in explicit {
            assert_eq!(
                mix("minecraft:potion", Some(id("awkward")), ingredient),
                Some(("minecraft:potion".to_owned(), Some(id(potion)))),
                "{ingredient} -> {potion}"
            );
            assert_eq!(
                mix("minecraft:potion", Some(id("water")), ingredient),
                None,
                "{ingredient} has no water rule"
            );
        }
    }

    #[test]
    fn redstone_lengthens_and_glowstone_strengthens() {
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("swiftness")),
                "minecraft:redstone"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("long_swiftness"))))
        );
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("swiftness")),
                "minecraft:glowstone_dust"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("strong_swiftness"))))
        );
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("healing")),
                "minecraft:glowstone_dust"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("strong_healing"))))
        );
        assert_eq!(
            mix("minecraft:potion", Some(id("poison")), "minecraft:redstone"),
            Some(("minecraft:potion".to_owned(), Some(id("long_poison"))))
        );
    }

    #[test]
    fn fermented_spider_eye_inverts() {
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("swiftness")),
                "minecraft:fermented_spider_eye"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("slowness"))))
        );
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("night_vision")),
                "minecraft:fermented_spider_eye"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("invisibility"))))
        );
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("healing")),
                "minecraft:fermented_spider_eye"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("harming"))))
        );
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("water")),
                "minecraft:fermented_spider_eye"
            ),
            Some(("minecraft:potion".to_owned(), Some(id("weakness"))))
        );
    }

    #[test]
    fn gunpowder_makes_splash_and_dragon_breath_makes_lingering() {
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("swiftness")),
                "minecraft:gunpowder"
            ),
            Some(("minecraft:splash_potion".to_owned(), Some(id("swiftness"))))
        );
        assert_eq!(
            mix(
                "minecraft:splash_potion",
                Some(id("swiftness")),
                "minecraft:dragon_breath"
            ),
            Some((
                "minecraft:lingering_potion".to_owned(),
                Some(id("swiftness"))
            ))
        );
    }

    #[test]
    fn a_mix_that_does_not_exist_is_refused() {
        // Glowstone on an already-strong potion: no rule.
        assert_eq!(
            mix(
                "minecraft:potion",
                Some(id("strong_swiftness")),
                "minecraft:glowstone_dust"
            ),
            None
        );
        // Gunpowder only converts a drinkable bottle, not a splash.
        assert_eq!(
            mix(
                "minecraft:splash_potion",
                Some(id("swiftness")),
                "minecraft:gunpowder"
            ),
            None
        );
        // A glass bottle has no potion contents to mix.
        assert_eq!(
            mix("minecraft:glass_bottle", None, "minecraft:nether_wart"),
            None
        );
    }

    #[test]
    fn brewable_ingredients_cover_every_rule_and_fuel() {
        for ingredient in [
            "minecraft:nether_wart",
            "minecraft:glowstone_dust",
            "minecraft:redstone",
            "minecraft:gunpowder",
            "minecraft:dragon_breath",
            "minecraft:fermented_spider_eye",
            "minecraft:sugar",
            "minecraft:phantom_membrane",
        ] {
            assert!(is_ingredient(ingredient), "{ingredient} brews");
        }
        assert!(!is_ingredient("minecraft:dirt"));
        assert!(is_fuel("minecraft:blaze_powder"));
        assert!(!is_fuel("minecraft:coal"));
    }

    #[test]
    fn splash_falloff_drops_short_effects() {
        // At the centre, full duration.
        let full = effects_of_scaled(id("swiftness"), 1.0);
        assert_eq!(full[0].duration, 3600);
        // At the rim, factor near zero: the effect is under 20 ticks and gone.
        assert!(effects_of_scaled(id("swiftness"), 0.0).is_empty());
        // Half strength: half duration.
        let half = effects_of_scaled(id("swiftness"), 0.5);
        assert_eq!(half[0].duration, 1800);
        // An instant effect is always kept, whatever the falloff.
        assert_eq!(effects_of_scaled(id("harming"), 0.01).len(), 1);
    }

    #[test]
    fn instant_helpers_scale_the_vanilla_way() {
        assert_eq!(effects::instant_heal(0, 1.0), 4.0);
        assert_eq!(effects::instant_heal(1, 1.0), 8.0);
        assert_eq!(effects::instant_damage(0, 1.0), 6.0);
        assert_eq!(effects::instant_damage(1, 1.0), 12.0);
        assert_eq!(effects::instant_heal(0, 0.5), 2.0);
    }

    #[test]
    fn a_lingering_potions_effects_are_quartered() {
        // `Items.LINGERING_POTION` carries `POTION_DURATION_SCALE = 0.25`, and
        // `AreaEffectCloud` floors the scaled duration without dropping short
        // effects the way a splash does.
        assert_eq!(LINGERING_DURATION_SCALE, 0.25);
        assert_eq!(effects_of_lingering(id("swiftness"))[0].duration, 900);
        assert_eq!(effects_of_lingering(id("strong_slowness"))[0].duration, 100);
        assert_eq!(effects_of_lingering(id("strong_poison"))[0].duration, 108);
        assert_eq!(effects_of_lingering(id("harming")).len(), 1);
    }

    #[test]
    fn potion_colours_are_the_amplifier_weighted_effect_colours() {
        assert_eq!(potion_color(id("water")), BASE_POTION_COLOR);
        // One effect is its own colour with full alpha.
        assert_eq!(potion_color(id("swiftness")) as u32, 0xff33ebff);
        assert_eq!(potion_color(id("slowness")) as u32, 0xff8bafe0);
        assert_eq!(potion_color(id("harming")) as u32, 0xffa9656a);
    }

    #[test]
    fn only_a_potion_with_an_instant_effect_uses_the_instant_event() {
        assert!(has_instant_effects(id("harming")));
        assert!(has_instant_effects(id("strong_healing")));
        assert!(!has_instant_effects(id("swiftness")));
        assert!(!has_instant_effects(id("water")));
        assert!(!has_instant_effects(id("poison")));
    }
}
