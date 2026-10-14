# QK notes — tasks J3 and J4 (items + behaviour)

Branch `oc/QK`, worktree `/srv/dev/projects/Aether-Engine-wt/QK`.

## Coordination with `oc/QJ`

Queue J was split while both agents worked. `oc/QJ` keeps J1/J2 (block
translation and substitution) and J5; its `f30e122` commit says explicitly that
J3/J4 "move to another branch" — this one. Earlier `oc/QJ` had briefly carried
a J3 item layer (`eae675a`), which it then removed. This branch is therefore
built from the shared base (`8b4a1c1`), owning J3 and J4, and not from QJ's
J1/J2, so the two branches merge as siblings rather than one duplicating the
other.

Consequence, exactly as the task described: **block items use a placeholder**
until the block rules merge in. The generator
(`tools/gen/compat/gen_items.js`) maps a block item a version lacks to a
generic stand-in (`barrier`) and records it; the TODO there says to resolve it
through the merged block rules (`compat::blocks`, `tools/gen/compat/blocks.json`)
and regenerate `compat/item_gen/`. Likewise the runtime does not call
`compat::blocks::*` yet.

## J3 — item tables and lossless item substitution

* `tools/gen/compat/items.json` — item stand-in rules (`*_spear → trident →
  diamond_sword` on 1.8.9, `mace → iron_axe`, `wind_charge → snowball`,
  `breeze_rod → blaze_rod`, `*_bundle → bundle → leather`, spawn eggs, pottery
  sherds, smithing templates, music discs, …).
* `tools/gen/compat/gen_items.js` + generated `compat/item_gen/*.rs` and
  `english.rs` — per-release `ITEM_MAP` / `ITEM_BY_ID` / `ITEM_FALLBACKS`
  (engine id → wire id, substitute bit 16) for every codec through 1.21.9.
* `compat/items.rs` — `ItemTable::{wire, name, engine, resolve_reported}`;
  `english()` for the display name.
* Modern `write_stack` now shows a stand-in carrying the real English name and
  an `Aether: minecraft:<id>` lore line instead of an empty slot. The
  `minecraft:lore` component id (11) is generated per version (`versions.py`,
  the four `modern/gen/*` files, `Version::component_lore`).
* Lossless round trip: `Codec::resolve_reported_item` + the hook in
  `session::record_slot_change` keep the server's real stack when a client
  echoes a substitute back.

## J4 — behaviour-preserving stand-ins

* `compat/behaviour.rs` — `Kind` (Plain/Spear/Mace/WindCharge), `Input`,
  `Action`, `Outcome::{action, resend_slot}`, `route`, `swallows_release`.
* The real item decides: `interact::use_item` starts the spear's charge,
  `interact::release_use` runs `spear_charge` (lunge + kinetic hit + arm
  animation), never a projectile; `tables::weapon` gains spear stats.
* Client correction: `correct_release` resends the slot (and the inventory
  resync) so the client's predicted trident throw is cancelled; `charging` is
  cleared on a hotbar change.
* The v767 (1.21.1) codec now decodes `use_item` and the release-use status of
  Player Action so the spear test can drive it. It still does not run the
  gameplay path — that is J5.

## Checks

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -j4 --
-D warnings`, `cargo test -p aether-server` all pass.
