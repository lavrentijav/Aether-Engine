# U1–U2 — loot tables and recipe coverage

Read `docs/tasks/COMMON.md`. Vanilla 1.21.11 data pack is the source (the worldgen code already
reads the pack: see `crates/aether-worldgen/src/vanilla` `DataPack`). Do not edit redstone, compat,
plugins, visibility, brewing, pathfinding or light code (other agents).

## U1 — loot tables
There are no loot tables in the engine: monster-room and igloo chests are empty, mobs and blocks drop
hand-written items. Implement the vanilla loot table system from `data/minecraft/loot_table/**.json`:
pools, rolls (constant/uniform/binomial), entries (item, tag, loot_table, alternatives, group,
sequence, empty), conditions (random_chance, random_chance_with_enchanted_bonus, killed_by_player,
survives_explosion, match_tool incl. silk touch / fortune predicates, block_state_property,
entity_properties basic), functions (set_count, set_damage, enchant_randomly, enchant_with_levels
(if the enchantment registry exists in main; otherwise a TODO), apply_bonus (ore_drops,
uniform_bonus_count, binomial), explosion_decay, furnace_smelt, looting/enchanted_count_increase,
limit_count, set_components minimal). Wire: block drops (replace hand-written tables where a loot
table exists), mob drops, chest loot for monster_room/igloo/structures (the `LootTable` string on the
chest, rolled on first open with a seeded RNG as vanilla). Tests: diamond ore with/without silk touch
and fortune III distribution, zombie drops with looting, dungeon chest rolls deterministic per seed.

## U2 — recipes coverage
Audit crafting/smelting/blasting/smoking/campfire/stonecutting/smithing against
`data/minecraft/recipe/*.json`: load recipes from the pack (shaped incl. mirrored, shapeless, tags,
remainders like buckets, special recipes listed and stubbed with a note), replace any hand-written
table, make the recipe book unlock packet optional/unsupported-but-harmless. Report counts: total in
pack, supported, special/unsupported (listed). Tests: a sample per type, tag ingredient, mirrored
shaped, remainder.

## Done
Per COMMON.md; update STATUS/KNOWN_ISSUES (EN+RU). Report hashes, tests, what is left.
