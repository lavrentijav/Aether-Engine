//! What mining and playing are worth.
//!
//! Two ways coins enter a survival world, and both are deliberately narrow.
//!
//! # Mining
//!
//! A broken block pays [`block_value`], but only when **the generator put it
//! there**. Anything a player placed pays nothing, and the check is not a
//! nicety: without it the cheapest exploit in the game is to place a diamond
//! block and break it, forever. The world history already knows the answer —
//! a position with no [`aether_world::journal`] entry is exactly one the
//! generator made — so the check is a lookup rather than a heuristic.
//!
//! Cheap, abundant blocks pay **zero**. Not one coin, zero: a rate of one per
//! dirt block means a player with a shovel out-earns a player with a diamond
//! pickaxe, because there is far more dirt than there is diamond. The scarce
//! blocks are the ones worth paying for, and the table below is sorted by how
//! hard the world makes them to find.
//!
//! # Time
//!
//! [`ONLINE_REWARD`] every [`ONLINE_INTERVAL`], counted from the moment a
//! player joins. Being present is the one thing every player can do equally,
//! so it is the floor under the economy — enough to trade with, never enough
//! to compete with mining.

use crate::economy::Money;

/// Paid for each completed interval of connected time.
pub const ONLINE_REWARD: Money = Money(25_000);

/// How long that interval is.
pub const ONLINE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

/// What one broken block pays, in minor units.
///
/// Everything absent from this table is worth nothing, which is the right
/// default: the table names what is scarce, and scarcity is the exception.
/// Values are per block, so the ore rows are what a player actually earns per
/// swing — deliberately small, because a vein is several blocks and a cave is
/// several veins.
const VALUES: [(&str, i64); 34] = [
    // Deep and dangerous.
    ("minecraft:ancient_debris", 50_000),
    ("minecraft:emerald_ore", 12_000),
    ("minecraft:deepslate_emerald_ore", 14_000),
    ("minecraft:diamond_ore", 10_000),
    ("minecraft:deepslate_diamond_ore", 12_000),
    // Valuable, still uncommon.
    ("minecraft:gold_ore", 3_000),
    ("minecraft:deepslate_gold_ore", 3_500),
    ("minecraft:nether_gold_ore", 2_000),
    ("minecraft:lapis_ore", 2_500),
    ("minecraft:deepslate_lapis_ore", 3_000),
    ("minecraft:redstone_ore", 1_500),
    ("minecraft:deepslate_redstone_ore", 1_800),
    // Common ores: worth something, not worth much.
    ("minecraft:iron_ore", 800),
    ("minecraft:deepslate_iron_ore", 900),
    ("minecraft:copper_ore", 400),
    ("minecraft:deepslate_copper_ore", 450),
    ("minecraft:coal_ore", 300),
    ("minecraft:deepslate_coal_ore", 350),
    ("minecraft:nether_quartz_ore", 500),
    // Not ore, but scarce enough to be worth finding.
    ("minecraft:amethyst_cluster", 1_200),
    ("minecraft:budding_amethyst", 5_000),
    ("minecraft:glowstone", 600),
    ("minecraft:sea_lantern", 1_500),
    ("minecraft:prismarine", 700),
    ("minecraft:obsidian", 900),
    ("minecraft:crying_obsidian", 4_000),
    ("minecraft:sponge", 6_000),
    ("minecraft:wet_sponge", 6_000),
    ("minecraft:bone_block", 400),
    ("minecraft:blue_ice", 800),
    ("minecraft:packed_ice", 300),
    ("minecraft:mycelium", 500),
    ("minecraft:warped_wart_block", 400),
    ("minecraft:nether_wart_block", 400),
];

/// What breaking `block` is worth, before the generated-or-placed check.
///
/// Zero for anything the table does not name — see the module docs on why the
/// abundant blocks pay nothing at all rather than a little.
pub fn block_value(block: &str) -> Money {
    // The name may carry state properties (`minecraft:redstone_ore[lit=true]`),
    // which never change what a block is worth.
    let bare = block.split_once('[').map_or(block, |(n, _)| n);
    Money(
        VALUES
            .iter()
            .find(|(n, _)| *n == bare)
            .map(|(_, v)| *v)
            .unwrap_or(0),
    )
}

/// Whether this block is worth anything at all — a cheap pre-check that
/// avoids a history lookup for the dirt and stone that make up most of a
/// player's mining.
pub fn is_payable(block: &str) -> bool {
    block_value(block) > Money::ZERO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_abundant_blocks_pay_nothing() {
        // Not "a little": a coin per dirt block means a shovel out-earns a
        // diamond pickaxe, because the world is mostly dirt.
        for cheap in [
            "minecraft:dirt",
            "minecraft:stone",
            "minecraft:deepslate",
            "minecraft:grass_block",
            "minecraft:sand",
            "minecraft:gravel",
            "minecraft:netherrack",
            "minecraft:cobblestone",
            "minecraft:oak_log",
            "minecraft:air",
        ] {
            assert_eq!(block_value(cheap), Money::ZERO, "{cheap} should be free");
            assert!(!is_payable(cheap));
        }
    }

    #[test]
    fn scarcity_and_price_agree() {
        // The ordering is the claim the table makes; a row edited to the wrong
        // magnitude breaks it silently otherwise.
        let v = |n| block_value(n).0;
        assert!(v("minecraft:ancient_debris") > v("minecraft:diamond_ore"));
        assert!(v("minecraft:diamond_ore") > v("minecraft:gold_ore"));
        assert!(v("minecraft:gold_ore") > v("minecraft:iron_ore"));
        assert!(v("minecraft:iron_ore") > v("minecraft:coal_ore"));
        assert!(v("minecraft:coal_ore") > 0);
    }

    #[test]
    fn deepslate_variants_are_worth_at_least_their_stone_twins() {
        // They are deeper and slower to mine, so paying less for them would
        // make the harder half of the world the worse place to dig.
        for (stone, deep) in [
            ("minecraft:diamond_ore", "minecraft:deepslate_diamond_ore"),
            ("minecraft:gold_ore", "minecraft:deepslate_gold_ore"),
            ("minecraft:iron_ore", "minecraft:deepslate_iron_ore"),
            ("minecraft:coal_ore", "minecraft:deepslate_coal_ore"),
            ("minecraft:lapis_ore", "minecraft:deepslate_lapis_ore"),
            ("minecraft:redstone_ore", "minecraft:deepslate_redstone_ore"),
            ("minecraft:copper_ore", "minecraft:deepslate_copper_ore"),
        ] {
            assert!(block_value(deep) >= block_value(stone), "{deep} vs {stone}");
        }
    }

    #[test]
    fn state_properties_do_not_change_what_a_block_is_worth() {
        // `redstone_ore` is `lit=true` the instant it is touched, and paying
        // nothing for a lit one would make redstone free by accident.
        assert_eq!(
            block_value("minecraft:redstone_ore[lit=true]"),
            block_value("minecraft:redstone_ore")
        );
        assert!(is_payable("minecraft:redstone_ore[lit=false]"));
    }

    #[test]
    fn every_row_names_a_real_item_and_is_worth_something() {
        // A typo in a block name is a row that can never pay out, and nothing
        // else would ever report it.
        for (name, value) in VALUES {
            assert!(value > 0, "{name} is in the table but pays nothing");
            assert!(
                crate::protocol::modern::items::item_id(name).is_some(),
                "{name} is not an item in 1.21.11 — is the name right?"
            );
        }
    }

    #[test]
    fn no_row_is_listed_twice() {
        let mut names: Vec<&str> = VALUES.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "a duplicated row shadows the other");
    }

    #[test]
    fn ten_minutes_of_presence_is_worth_two_hundred_and_fifty_coins() {
        assert_eq!(ONLINE_REWARD.to_string(), "250.00");
        assert_eq!(ONLINE_INTERVAL.as_secs(), 600);
    }
}
