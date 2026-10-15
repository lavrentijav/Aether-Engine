Review continued against the decompiled `net/minecraft/world/level/storage/loot` classes and the pack. Re-ran `cargo-locked test -p aether-server --bin aether-server game::loot` with the prescribed env: 9 passed (including `every_real_table_parses`). Passing tests do not cover the defects below; several actively enshrine the wrong shape.

---

**1. `alternatives` children re-test RNG conditions, squaring their chance — reachable drop loss. High.**
`crates/aether-server/src/game/loot.rs:356-360` runs `.find(|c| matches_all(&c.conditions, ctx, rng))`, then `run_entry(child)` (`:324`) calls `matches_all` on the same child again. Any RNG-consuming condition (`random_chance`, `table_bonus`, `survives_explosion` under an explosion) is drawn twice. Concrete, no enchantments needed: `data/minecraft/loot_table/blocks/short_grass.json` is `alternatives[ shears→grass, wheat_seeds{random_chance:0.125} ]`. By hand the engine's seed rate is 0.125² ≈ **1.56%** vs vanilla 12.5%; `blocks/fern.json` and `tall_grass`/`large_fern` likewise. Oak leaves (`blocks/oak_leaves.json`) sapling `table_bonus` 0.05 → 0.0025. This is the strongest reachable defect.

**2. `tag` with `expand:true` is inverted — turns one weighted pick into N guaranteed drops (duplication). High.**
`loot.rs:333-350`: `expand` → emit *all* members; `!expand` → pick one. Vanilla (`TagEntry.expand`/`expandTag`) is the opposite: `expand:true` makes each member a separate weighted candidate (one chosen), `false` emits all members. `entities/creeper.json` pool 2 is `tag minecraft:creeper_drop_music_discs, expand:true` → engine returns all 12 discs, vanilla one. Latent only because `attacker_kind` is never populated; untested.

**3. `limit_count` parses the wrong JSON path, so the function is a no-op in every table; the unit test asserts a non-vanilla body. High.**
`loot.rs:1274-1276` reads top-level `min`/`max`, but vanilla wraps them in `"limit": {"min"…,"max"…}` (`LimitCount.CODEC` = `IntRange.CODEC.fieldOf("limit")`). All 6 pack uses (`blocks/red_mushroom_block`, `brown_mushroom_block`, `melon`, `sea_lantern`, `glowstone`, `creaking_heart`) are silently unclamped. The test `set_count_and_limit_count` (`loot.rs:1433-1436`) feeds `{"function":"minecraft:limit_count","max":2}` — a shape vanilla never emits — so it passes while the real format does nothing. (Related: `set_count` default `add` is `true` at `loot.rs:1272`; vanilla `SetItemCountFunction` is `.orElse(false)`. Latent, no pack entry omits `add`.)

**4. `apply_bonus` `binomial_with_bonus_count` is the wrong formula. High.**
`loot.rs:547-555`: loops `level` times adding `extra`. Vanilla `ApplyBonusCount.BinomialWithBonusCount.calculateNewCount` loops `level + extraRounds` times adding 1. For `blocks/wheat.json`/`carrots`/`potatoes`/`beetroots` seeds (`extra:3, p:0.571`) with fortune III, vanilla mean ≈ 4.43/max 7; engine ≈ 6.14/max 10.

**5. `entity_properties` fails closed on `type_specific`/`components`/`equipment`/`vehicle`, deleting whole drop pools. Medium-High, reachable.**
`loot.rs:1175-1181` maps any such predicate to `Condition::Impossible`. `entities/sheep.json` pool 2 is an `alternatives` of 16 children all gated on `components`+`type_specific` → **sheep never drop wool**; `entities/slime.json` pool 0 is gated on `type_specific size:1` → **slimes never drop slime balls**; `entities/magma_cube.json` all entries gated → **no magma cream**. The built-in fallback also lacks wool, so this is a persistent vanilla divergence.

**6. Breaking an unopened generated chest loads, seeds and spills its loot. Medium.**
`crates/aether-server/src/game/containers.rs:384` (`remove`) → `with` → `load` (`:173`), which calls `seed_generated_chest` (`loot.rs:896`). `interact.rs:170` calls `remove` on any broken chest. Vanilla fills a chest only on a player open; here mining a never-opened dungeon/igloo chest drops up to ~10 generated stacks (and a hopper touching it seeds it too).

**7. `pick` ignores entry conditions, so a roll can pick an entry that then fails and yields nothing. Medium.**
`loot.rs:386` picks among all entries by weight; the chosen entry's conditions are only checked in `run_entry` (`:324`). Vanilla `LootPool.addRandomItem` excludes entries whose `expand`/`canRun` fails before selecting. Concrete: `gameplay/fishing/junk.json` bamboo has a `location_check` condition (→ Impossible); engine sometimes picks bamboo and drops nothing where vanilla renormalises to another junk item. `entities/magma_cube.json`/`slime.json` same pattern.

**8. `enchanted_count_increase` draws an integer then multiplies, not `round(level × getFloat)`. Medium.**
`loot.rs:571-586`. Vanilla `EnchantedCountIncreaseFunction.run` = `grow(Math.round(level * value.getFloat))`. For the zombie's `uniform 0..1`, Looting III here gives only 0 or 3 (mean 1.5), vanilla 0/1/2/3 with probs 1/6,1/3,1/3,1/6. The test `zombie_looting_raises_the_drop` (`:1467`) asserts only mean/max — which coincide — so it locks in the wrong distribution.

**9. The parser silently discards unimplemented constructs; `unsupported()` is always empty at startup, and table-level functions are never read. Medium.**
`parse_conditions` (`loot.rs:1050`) turns unknown conditions into `Impossible` and `parse_functions` (`:1247`) `filter_map`s unknown functions away, neither recording anything; `unsupported` is written only when a whole table fails to parse (`:760`), and `main.rs:177` calls it before the lazy `table()` calls, so the "unsupported construct(s)" log never fires. That hides 71 `copy_components`, 48 `set_potion`, 11 `set_enchantments`, and every fail-closed predicate — contradicting the commit and `KNOWN_ISSUES A.10`. Separately, `parse_table` (`:930-938`) reads only `pools`, so root-level `functions` (9 block tables, e.g. `blocks/wheat.json`'s `explosion_decay`) are dropped entirely.

**10. A `loot_table` reference cycle recurses without bound → stack overflow/abort on a malformed pack. Medium.**
`LootTables::table` (`loot.rs:700`) caches only after `parse` returns, and `resolve_nested` (`:770`) calls `table()` again; the `depth > 4` guard (`:771`, `:783`) resets on each table reference. Table A → B → A re-parses A forever. No cycle exists in the vanilla pack, but this is a hard crash (not a handled error) on operator-supplied data, and vanilla protects against it via visited-entry detection.

---

Verdict: **DO NOT MERGE** — findings 1–4 are wrong implementations of features the task explicitly requires, #1 and #5 are reachable live item loss, and tests #3/#8 assert behaviour vanilla never has.
