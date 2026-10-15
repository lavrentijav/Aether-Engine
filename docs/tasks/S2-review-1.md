Reviewed `8d069f6` against the 1.21.11 decompiled sources and the shipped 26.1.2/26.2 jars; `cargo-locked test -p aether-server` passes (787), but the tests never exercise the paths below.

## Findings

**1. HIGH — wrong `potion_contents` component id for every 26.x codec.**
`crates/aether-server/src/protocol/modern/gen/v26_1.rs:1774`, `v26_2.rs:1860`, `v26_3.rs:2016` set `COMPONENT_POTION_CONTENTS = 49`. In 1.21.11 that is correct, but in 26.1.2/26.2 the registry puts `POTION_CONTENTS` at **51** (49 = `charged_projectiles`, 50 = `bundle_contents`), verified by dumping `DataComponents.class` from both merged jars (`DAMAGE`/`LORE`/`CUSTOM_NAME` still 3/11/6, so the counting method is sound). Scenario: a 26.1/26.2 client is sent any stack holding a potion (inventory sync, window, ground item) — the server writes type 49 with a `PotionContents` payload, the client decodes it as `charged_projectiles`, and the malformed payload throws the item codec → "Network Protocol Error"/kick or a corrupt item. The generator (`tools/gen/versions.py:100`) derives this from the version's own `registries.json`, so the committed values are stale for 26.x; the only test (`play.rs` `a_potion_stack_carries_potion_contents`) pins `V1_21_11`, so it cannot catch it.

**2. HIGH — lingering clouds apply effects at full duration, not ×0.25.**
`crates/aether-server/src/game/entities.rs:1410` (`cloud_pulse`) → `:1439` uses `brewing::effects_of(potion)` (full registry duration). Vanilla's `AreaEffectCloud` is built from a `lingering_potion`, which carries `POTION_DURATION_SCALE = 0.25F` (`Items.java:1915`), and `serverTick` calls `forEachEffect(..., potionDurationScale)` → durations are quartered. `KNOWN_ISSUES` does not mention this. Scenario: throw a lingering Swiftness (3600) cloud — vanilla gives 900 t (45 s), this gives 3600 t (3 min); every lingering effect is 4× too long.

**3. MEDIUM — lingering potions also fire a full splash on impact.**
`crates/aether-server/src/game/entities.rs:1397-1399`: for `lingering` it spawns the cloud **and then unconditionally** calls `apply_potion_area(..., splash=true, 4.0)`. Vanilla `ThrownLingeringPotion.onHitAsPotion` only creates the cloud; nothing is applied to entities at the impact point. Scenario: a lingering Harming landing on a player deals an immediate 6 damage (plus the cloud's own), which vanilla never does.

**4. MEDIUM — cloud instant effects use factor 1.0 instead of vanilla's 0.5.**
`entities.rs:1404` passes `splash=false`, so `apply_potion_area` takes `factor = 1.0` (`:1434`) and feeds it to `instant_heal`/`instant_damage` (`:1469`, `:1472`, `:1501`, `:1507`). Vanilla `AreaEffectCloud.serverTick` calls `applyInstantenousEffect(..., 0.5)`. Scenario: a lingering Harming/Healing cloud heals/hurts for double the vanilla amount on every pulse.

**5. MEDIUM — instant health/damage is never inverted for the undead.**
`entities.rs:1466-1476` (mob branch) decides purely `INSTANT_HEALTH → heal else damage`. Vanilla `HealOrHarmMobEffect.applyInstantenousEffect` checks `livingEntity.isInvertedHealAndHarm()`. The engine already models this (`MobDef.undead`, `mobs.rs:29`, true for zombie/skeleton). Scenario: a splash Harming potion on a zombie should heal it and a splash Healing should hurt it; here it does the opposite.

**6. LOW/MEDIUM — a brewing stand with residual fuel never leaves the active set.**
`containers.rs:546` sets `*idle` only when `burn == 0 && cook == 0`; unlike `tick_furnace`, `burn` only decreases when a brew starts. A stand that once had blaze powder (fuel 1–20) and no valid bottles stays in `active()` forever, locking the global container map and running `tick_brewing` every tick until restart or break. Furnaces do not have this because their fuel burns down.

**7. LOW — bottle filling accepts flowing water and rejects waterlogged blocks.**
`crates/aether-server/src/game/interact.rs:449-452`: raycast stops on any non-air block and accepts `block_name == "minecraft:water"`, which includes flowing water. Vanilla `BottleItem` uses `getPlayerPOVHitResult(..., ClipContext.Fluid.SOURCE_ONLY)`: flowing water must not fill, waterlogged source blocks must. Scenario: a glass bottle fills from a flowing stream (duplicating water into a bottle where vanilla refuses).

**8. LOW — splash/lingering impact always sends world event 2002 with data 0.**
`entities.rs:1388`: vanilla uses 2007 for a potion with instant effects and passes `potionContents.getColor()` as the data. Scenario: instant potions play the wrong particles/sound and never the instant-effect colour cue; purely cosmetic.

**9. LOW — the item journal cannot see a brew change a potion.**
`crates/aether-server/src/game/provenance.rs:49-52`: `Entry` records only `place/count/item`, not `potion`, and `do_brew` keeps the uid (`containers.rs:600+`). A water→awkward change keeps the same uid/item/count, so `container_changed`'s `seen(before) == seen(after)` and `diff` is empty — no `ItemDerive`/`ItemMove` event, unlike every other item transformation. Audit/rollback lose the fact that the bottle's contents changed.

## Verdict
**DO NOT MERGE** — the 26.x component id breaks potions on clients the codec claims to support, and the lingering-cloud duration/scaling/extra-splash trio is a flat behaviour divergence from vanilla.
