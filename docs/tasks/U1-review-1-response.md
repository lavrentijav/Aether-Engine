# U1 review 1 — response

Review: `docs/tasks/U1-review-1.md`. Every finding was checked against the pack
(`/tmp/opencode/mc-pack`, 1.21.11) and the vanilla `net.minecraft.world.level.storage.loot`
semantics. All ten are fixed, one commit each. Full workspace `cargo test`, `clippy -D
warnings` and `fmt --check` are green, with `CARGO_INCREMENTAL=0`.

| # | Verdict | Fix commit | Notes |
|---|---------|-----------|-------|
| 1 | Confirmed | `a7e1493` U1-fix1 | `alternatives` re-tested the chosen child's conditions. Rewrote the roll to vanilla expand-then-weight: conditions once. |
| 2 | Confirmed | `c4ea07e` U1-fix2 | `tag` `expand` was inverted. Corrected in the fix1 rewrite; tests pin both directions. |
| 3 | Confirmed | `dcab6e3` U1-fix3 | `limit_count` reads under `limit`; `set_count` `add` defaults false. |
| 4 | Confirmed | `3210c8e` U1-fix4 | Binomial loops `level + extra`, +1 each. |
| 5 | Partly confirmed | `a46b8ea` U1-fix5 | Reachable half was sheep; see rebuttal. Slime/magma are not engine mobs. |
| 6 | Confirmed | `85b09ba` U1-fix6 | Seeding moved out of `load` to the open path. |
| 7 | Confirmed | `1828ef2` U1-fix7 | Fixed by the fix1 rewrite; test pins the fishing-junk case. |
| 8 | Confirmed | `1909c9f` U1-fix8 | `round(level * getFloat)`, bonus clamped to `limit`. |
| 9 | Confirmed | `3afe72f` U1-fix9 | Skipped constructs recorded; root-level `functions` parsed and applied. |
| 10 | Confirmed | `9a273ec` U1-fix10 | Reference cycles cut via the resolution chain. |

## Rebuttals / corrections

* **Finding 5, "sheep never drop wool": inaccurate as stated.** `game::combat::on_mob_hurt`
  pushed a hard-coded `white_wool` for every sheep, so wool *did* drop — but always one
  white, regardless of the sheep's colour or whether it was sheared, and independent of
  the table. The underlying defect (whole pools deleted by an unhandled predicate) was
  real, and is fixed properly: `components`/`type_specific` are evaluated, the manual
  wool push is removed, and the tests cover white/black/sheared. Slime and magma cube
  pools were latent, not reachable — neither is one of the engine's eight mobs.

* **Finding 2 was latent, not live.** The creeper's music-disc pool is gated on
  `attacker: #minecraft:skeletons`, and the engine never populates `attacker_kind`, so the
  inverted `expand` never produced all twelve discs in a live session. The semantics were
  still wrong and are fixed; fixed close to the fix1 rewrite, and pinned by tests.

## Behaviour now (all pack-gated)

* `short_grass` seeds at 0.125 (was 0.125²).
* Fortune III melon slices 3..=9 and wheat seeds mean ≈4.43 / max 7.
* Sheep wool by colour, none when sheared; slime/magma pools resolve when a context
  supplies `type_specific`.
* Zombie looting III adds 0/1/2/3 at 1/6, 1/3, 1/3, 1/6.
* Dungeon/igloo chests are filled only on a player open, and roll the same for a given
  seed+position.
* All 1,324 pack loot tables parse; every skipped construct is reported once at startup.
