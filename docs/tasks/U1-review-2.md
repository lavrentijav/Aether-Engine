Verified against the pack and decompiled `net/minecraft/world/level/storage/loot`. All 24 loot tests and the full 791-test `aether-server` suite pass with `AETHER_VANILLA_PACK` set (`cargo-locked test`, `CARGO_INCREMENTAL=0`).

## Findings from `docs/tasks/U1-review-1.md`

**1. `alternatives` re-tested RNG conditions — FIXED.** The roll now expands every pool entry to singleton `Candidate`s (conditions once) then weights survivors (`loot.rs:317-390`). `expand_entry_unchecked` is used for the chosen `alternatives`/`sequence` child, so no second `matches_all`. `alternatives_evaluate_a_conditions_once` and `real_short_grass_seed_rate_is_vanilla` (0.125) pin it.

**2. `tag` `expand` inverted — FIXED.** `expand:true` now emits one candidate per member; `expand:false` one candidate containing all members (`loot.rs:445-465`). Matches `TagEntry.expandTag`/`super.expand`. `tag_expand_false_emits_every_member` and `real_creeper_tag_yields_one_disc` cover both.

**3. `limit_count` path / `set_count` default — FIXED.** Bounds read from `limit` (`loot.rs:1583-1596`); `add` defaults false (`:1580`). `real_melon_limit_count_clamps` passes.

**4. `binomial_with_bonus_count` — FIXED.** Loops `level + extra`, `+1` each (`loot.rs:695-701`), per `ApplyBonusCount.BinomialWithBonusCount`. `real_wheat_seeds_match_binomial_bonus` (mean ≈4.43, max 7) passes.

**5. `entity_properties` components/type_specific — PARTIAL.** The reachable half is fixed: `components` and `type_specific` are parsed/evaluated (`loot.rs:1204-1229`, `:585-608`), combat supplies sheep colour/sheared, the hard-coded white wool is gone, and `real_sheep_wool_follows_colour_and_shear` passes. But the response/notes claim "slime/magma pools resolve" — false: `magma_cube.json` uses `"size": {"min": 2}` (a range object), which the parser rejects as `Impossible` (`:1223-1227`), and `equipment`/`vehicle` predicates (cow/pig/chicken smelting alt, zombie mushroom) still fail closed. All latent (none are engine mobs), but the stated coverage is overstated.

**6. Chest seeded on break — FIXED (exploit), NEW BUG introduced.** Seeding moved out of `load` to the player-open path (`containers.rs:170-196`, `interact.rs:247-252`); `remove`/`load` no longer seed. See N1 below.

**7. `pick` ignored conditions — FIXED.** Conditions are evaluated during expansion, so a roll cannot select an entry that then fails; `real_fishing_junk_excludes_unrunnable_entries` pins the bamboo `location_check` case.

**8. `enchanted_count_increase` — PARTIAL.** Distribution is now `round(level × getFloat)` (`loot.rs:735-750`) and `enchanted_count_increase_is_round_level_times_float` pins 1/6,1/3,1/3,1/6. But `limit` is applied to the *bonus*, not the total — see N2.

**9. skipped constructs / root functions — PARTIAL.** Unknown condition/function/entry types are recorded via `note` and `warm()` runs before the report (`loot.rs:1129-1140`, `main.rs:176-191`); root-level `functions` are parsed and applied (`:1055`, `:379-383`). But the fail-closed *predicate keys* (unknown `match_tool` keys, `equipment`/`vehicle`/`nbt`, unknown flags) return `Condition::Impossible` with no `note` (e.g. `:1150-1160`, `:1198-1205`), so they never appear in `unsupported()` — contradicting KNOWN_ISSUES "every … unmodelled predicate is recorded". See N4.

**10. reference cycle — FIXED.** `table_impl` carries a resolution `stack` and cuts re-entry, recording `…: cyclic loot_table reference` (`loot.rs:722-744`). `cyclic_loot_table_references_do_not_recurse` passes; no cycle in the pack.

## New defects in the fixes

**N1. Stale container cache silently voids first-open seeding. Medium.**
`interact.rs:249` writes the seeded loot to meta only; it does not touch the in-memory `all()` map. But `containers::load` is reached by non-open paths (`move_one` at `containers.rs:238` inserts `or_insert_with(load)`) that populate `all()` with the *empty* chest. `window::open` then uses `containers::with`/`with_pair`, which prefer `all()` over meta (`containers.rs:200`), so the player sees an empty chest and subsequently re-encodes the empty container back over the seed. Scenario: a player places a hopper adjacent to a generated dungeon chest; the hopper's pull populates the cache; the chest then opens empty and stays empty across restarts. (Separately, vanilla generates on any first `getItem`/`removeItem`/`isEmpty` — including hoppers — so the player-only rule is itself a divergence, though documented.)

**N2. `enchanted_count_increase` clamp caps the bonus, not the stack. Medium-low, latent.**
`loot.rs:747-750` computes `add = extra.clamp(0, cap)` then adds. Vanilla does `grow(round(level×getFloat))` then `itemStack.limitSize(limit)` — capping the *total* (`ItemStack.limitSize`, verified). For `bogged`/`parched`/`stray` (`limit:1`, base `uniform 0..1`) the engine can drop 2 where vanilla caps at 1. None are engine mobs, so latent.

**N3. `NumberProvider::get_i32` Constant switched `round`→`floor`. Low (inert).**
`loot.rs:38` changed to `v.floor() as i32`; commit a7e1493 claims "vanilla's `getInt`", but vanilla's default is `Math.round(getFloat)` (`NumberProvider.getInt`), which `ConstantValue` does not override. No pack constant is fractional, so no effect today, but the rationale is wrong and it is now less faithful than before.

**N4. Unmodelled predicate keys are not recorded. Low-medium (observability).**
As in #9 above: the exact predicates that delete whole pools (`equipment`, `vehicle`, unknown flags, unknown `match_tool` keys) take the `return Some(Condition::Impossible)` branches without `note`, so `unsupported_constructs_are_recorded` passes while the pool-deleting gaps are absent from the startup report and from KNOWN_ISSUES' claim.

## Still standing from the first review (not in `U1-review-1.md`, unaddressed)

**S1. `explosion_decay` destroys the whole stack instead of decaying per item.** `loot.rs:708-714` keeps all-or-nothing; vanilla `ApplyExplosionDecay` rolls once per item and keeps each survivor. Latent — no caller ever sets `explosion_radius` (`interact.rs:184` builds only tool+block), so 147 tables incl. diamond-ore never exercise it.

**S2. Inline `loot_table` value yields nothing.** `parse_entry` (`loot.rs:1090-1094`) takes `str_of("value")`, and an object `value` becomes `""`. `equipment/trial_chamber.json` is entirely an inline value; it silently produces nothing (not even recorded). Latent (no trial chambers).

Minor/inert: `pick_candidate` returns `candidates.first()` on total weight 0 where vanilla produces nothing (no zero-weight entries in the pack); `set_count` clamps negatives to 0 before `limit_count` (same net result).

## Merge prerequisite

`oc/U1` has not merged `main` (`git merge-base --is-ancestor main HEAD` fails). `main` moved and split `aether-server` into lib + bin; this branch is still bin-only (`no src/lib.rs`). The branch must be merged/replayed onto current `main` (the `combat.rs` / `entities.rs` / `main.rs` edits and `crate::worldgen_data()` will need the lib/bin rearrangement) before it can land. Not a defect in the change itself.

## Verdict

**DO NOT MERGE** — the ten reviewed findings are fixed or acceptably partial, but fix6 introduced a real loot-voiding bug (N1) and fix8's `limit` handling is wrong (N2); N4 additionally makes the KNOWN_ISSUES claim inaccurate, and the branch is not rebased on the lib/bin `main`.
