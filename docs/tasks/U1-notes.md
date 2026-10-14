# U1 — loot tables: notes

Read `docs/tasks/COMMON.md` and `QUEUE-U-loot-recipes.md`. Branch `oc/U1`.

## Scope delivered

`crates/aether-server/src/game/loot.rs` implements the vanilla loot table system,
reading the operator's pack (`data/minecraft/loot_table/**.json`) through the same
`worldgen_data` path the generator uses.

* Tables: pools, `rolls`/`bonus_rolls` (constant/uniform/binomial), weighted entries,
  `alternatives`/`group`/`sequence`/`empty`, item/tag (expanded and random)/nested
  `loot_table` entries, entry- and function-level `conditions`.
* Conditions: `random_chance`, `random_chance_with_enchanted_bonus`, `table_bonus`,
  `killed_by_player`, `survives_explosion`, `match_tool` (item list/tag + enchantment
  levels, incl. silk-touch/fortune), `block_state_property`, basic `entity_properties`
  (type/tag + `is_on_fire`/`is_baby`/`is_sneaking`), `any_of`/`all_of`/`inverted`.
  Anything unmodelled fails **closed** and is counted, never silently passed.
* Functions: `set_count`, `limit_count`, `apply_bonus` (`ore_drops`,
  `uniform_bonus_count`, `binomial_with_bonus_count`), `explosion_decay`,
  `furnace_smelt`, `enchanted_count_increase`, plus parsed no-ops for
  `enchant_randomly`, `enchant_with_levels`, `set_damage`, `set_components`.

Wiring:

* Block drops: `tables::block_drops_ctx` rolls the block's table (state-aware, via
  `tables::block_state_context`) and falls back to the built-in table without a pack;
  `game::interact::on_broken` builds the full context.
* Mob drops: `tables::mob_drops_ctx` rolls the entity table with a context that carries
  `player_kill`/`looting`; the old path is the no-pack fallback.
* Chests: a naturally generated dungeon/igloo chest is filled on its first open
  (`game::containers::load` → `loot::seed_generated_chest`), seeded from world seed +
  position + table id so the same world rolls the same chest.

## Accepted deviations (recorded in KNOWN_ISSUES A.10)

1. The generator keeps no block entity on generated chests, so which table a chest
   carries is **inferred** from the structure around it (`spawner` → `simple_dungeon`;
   `brewing_stand`/`water_cauldron` → `igloo_chest`) rather than stored as the
   `LootTable` string. A position the journal has ever touched gets no loot.
2. Enchantments are not item components yet, so enchantment-writing functions are
   no-ops; an enchanted item drops plain.
3. Gameplay has no source of `silk_touch`/`fortune`/`looting`, so those predicates read
   zero in a live session — the engine computes them correctly when a context carries
   them (the tests do).
4. `bonus_rolls` is added without a luck factor (every table touched carries `0.0`).

## Tests

* Inline (always run): ore-drops fortune III distribution (mean 2.2, P(1)=2/5), zombie
  looting mean/max (2.5/5), weighted pools + `set_count`/`limit_count`,
  `survives_explosion`, determinism of a seeded chest roll.
* Pack-gated on `AETHER_VANILLA_PACK` (skips without it): real diamond-ore silk/fortune,
  real zombie drops with and without a player kill, a real chest rolled twice, and
  **all 1,324 pack loot tables parse**.

## Left

* Persisting generated chest loot tables at generation time (a real block-entity channel
  through worldgen) instead of inferring them.
* Enchantment item components, which would make the enchant functions real and give
  gameplay a `fortune`/`looting` source.
* `dynamic` entries (decorated pot sherds) and block-entity `copy_components` produce
  nothing.
