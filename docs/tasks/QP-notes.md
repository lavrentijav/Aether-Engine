# QP notes — task J10 (block items follow block rules + per-move round trip)

Branch `oc/QP`, worktree `/srv/dev/projects/Aether-Engine-wt/QP`.

## Part 1 — block items follow the block rules

* `tools/gen/compat/gen.js` now exports its resolver, rule loader and name
  helpers and only generates when run directly (`require.main === module`).
  `tools/gen/compat/gen_items.js` requires it and resolves a block item
  (an engine item whose name is an engine block) through the same
  `blocks.json` rules and the same chained resolver the block half uses; the
  `barrier` placeholder is gone.
* A block no rule covers takes the version's **stone item** — the block
  table's own last resort — so `ITEM_FALLBACKS` holds only non-block items.
  Fallbacks dropped from 3919 to 623; 1.21.9 is 6.
* Regenerated `crates/aether-server/src/protocol/compat/item_gen/*.rs` (the
  block tables regenerate byte-identical; `english.rs` unchanged).
* Test `no_block_item_is_left_without_a_stand_in` asserts no fallback names an
  engine block in any release; `how_many_items_still_have_no_stand_in_is_recorded`
  now pins the much smaller counts.

## Part 2 — the round trip follows a move, not just a slot

* `compat::items` gains `ItemTable::stand_in_wire` (is a wire id ambiguous?),
  `resolve_move` (resolve a report against the window's real items),
  `resolve_change` (the pure per-move decision, returning the resolution and
  the updated shadow), and `table_for(protocol)` (the table a codec serves).
* `session::record_slot_change` resolves each creative slot report through
  `table_for(codec.protocol_id())`, shadowing the creative cursor in the new
  `PlayerState::creative_carry`. A stand-in moved slot → cursor → slot keeps
  its real item; a stand-in with no source is refused and the client resynced.
  A version with no table (1.21.11 and later, whose ids are the engine's own)
  is trusted as before, still honouring `ProtocolCodec::resolve_reported_item`.
* Tests: `a_moved_stand_in_keeps_the_real_item_it_stands_for`,
  `a_stand_in_survives_being_carried_through_the_cursor_between_slots` (the
  slot → cursor → empty slot → another slot flow, plus the refused conjure and
  a genuine item still believed).

## Checks

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -j4 --
-D warnings`, `cargo-locked test -p aether-server` (796 passed) all pass.

## Merge with main (T4 + QO)

`git merge main` conflicted in `tools/gen/compat/gen.js` (T4 added `META_MAPS`
and the `meta`/`legacy` rule fields with a richer `resolver` returning
`{ name, rule, cap }`) and in `docs/KNOWN_ISSUES.md`. Resolved so both survive:

* `loadRules(file = 'blocks.json')` keeps the parameter (J10) and also parses
  `metaMaps` and the `meta`/`legacy` fields (T4); `main()` keeps the
  require-safe exports and the `flatRules = rules.filter(r => !r.legacy)`
  split, so a flattened version drops the legacy-only rules.
* `gen_items.js` follows the object-returning resolver (`resolveItem(name)?.name`)
  and passes all rules to 1.8 but the legacy-filtered set to flattened
  versions, so its block-item stand-ins exactly match the block tables'.
* Docs: kept T4's item 14 and J10's item 15; synced the Russian item 14 to
  T4's (main had updated only the English one and left the stale 422-block
  list).

`node tools/gen/compat/gen.js` regenerates `compat/gen/*` **byte-identical to
main's**; `gen_items.js` regenerates only `item_gen/v1_8.rs` differently from
the pre-merge J10 tables (the legacy rules), the flattened tables unchanged.

## Review fixes (QP-review-1, commits J10-fix1 … J10-fix7 + J10-fixtest)

`docs/tasks/QP-review-1.md` returned DO NOT MERGE with F1–F7. All are fixed,
one commit each, with a test reproducing the review's sequence:

* **F1** (`J10-fix1`): a report with no source is a *creation* — the genuine
  item the wire id names — not a refusal. `Resolution::Refuse` removed.
* **F2** (`J10-fix2`): the source is only the single carried shadow, and it is
  spent; `resolve_move`'s all-slots scan (which never consumed the source) is
  gone.
* **F3** (`J10-fix3`): `PlayerState::clear_creative_shadow` is called by
  `combat::die`, `combat::respawn` and `game::window::close`.
* **F4** (`J10-fix4`): an empty report for an already-empty slot leaves the
  shadow alone (a genuine pickup supersedes it).
* **F5** (`J10-fix5`): a block item resolves through `blocks.json` first and
  only; the three block-item entries in `items.json` are removed.
* **F6** (`J10-fix6`): the source is the carried item, never the lowest
  matching slot (fix2 already removed the scan).
* **F7** (`J10-fix7`): `LEGACY_BLOCK_ITEMS` maps `standing_sign`/`wall_sign` →
  `sign`, the banner blocks → `banner`, etc. The campfire family still maps to
  stone (1.8 has no `fire` item) — asserted and documented.
* **property test** (`J10-fixtest`): random window reports never grow the spear
  multiset; pure moves conserve it exactly.

## Round-2 fixes (QP-review-2, J10-fix8 … J10-fix11)

`docs/tasks/QP-review-2.md` confirmed F1–F6 fixed and found N1–N3:

* **N1** (`J10-fix8`): the carried item now has priority over the slot's own
  value, so a swap of a stand-in onto a same-wire slot is a *move*, not an
  echo; and the empty-slot arm shadows *any* picked-up item, not only a
  disguised one. Test `a_swap_of_a_stand_in_onto_a_same_wire_slot_keeps_the_real_item`.
* **N2** (`Merge main` `18051ac` + `J10-fix9`): merged `main` (1.20.2/1.20.4
  gameplay, visibility, brewing) and added 764 → `V1_20_2`, 765 → `V1_20_4` to
  `table_for`, regenerating both item tables. Block tables byte-identical to
  `main`.
* **N3** (`J10-fix10`): the property test now drives a faithful client model
  (no ambiguous echo) and after every step asserts `server == client`,
  `shadow == cursor`, the full multiset never loses an entry, and the spear
  count is invariant.
* **F7** (`J10-fix11`): campfire/soul_campfire map to `flint_and_steel` on 1.8
  instead of stone (1.8 has no `fire` item); the residual — block `fire` vs item
  flint and steel — is documented.

## Round-3 fixes (QP-review-3, J10-fix12)

`docs/tasks/QP-review-3.md` confirmed N1–N3 fixed and found N4/N5:

* **N4** (`J10-fix12`): the fall-through branch now moves the replaced slot item
  to the cursor instead of dropping it, so a swap whose cursor the server never
  shadowed (a menu item conjured onto the cursor sends no packet) keeps the
  stand-in. The same-wire "echo" branch is gone — the replaced item is always
  shadowed.
* **N5** (`J10-fix12`): the shadow is now `Carried { item, count }`, so a
  partial placement spends only the items placed.
* **property test** (`J10-fix12`): `run_reports` gains a "conjure onto the
  cursor" op and asserts `server == client`, every disguised item is mirrored
  in slots+shadow, and the full multiset (count included) is conserved.

N4 and N5 share `resolve_change` and are one commit; reverting the N4 branch
makes both the sequence test and the property test fail.

## Round-4 fix (QP-fix13)

A stale cursor shadow could diverge the stand-in/real identity under a creative
cursor replacement (a conjure onto a non-empty cursor sends no packet). Fix:

* `resolve_change` now only trusts the shadow when the report **reconciles**
  with it (`reconciles`: same wire id and a count the shadow could carry —
  `r_count <= carried.count`, or a merge onto the same real item bounded by what
  that stack holds). A count above the shadow into an empty or foreign slot
  invalidates it and writes the *genuine* reported item — never the engine-only
  stand-in, never more of it than was carried.
* The echo guard (same-wire report with no shadow match) is restored, scoped to
  the no-carry case and to a count that did not grow, and now **discards** the
  shadow: the report is not the shadow's item, so the cursor is not what the
  server thought.

Tests: `a_conjure_onto_a_non_empty_cursor_invalidates_the_shadow`,
`a_stand_in_wire_conjure_cannot_materialise_the_engine_item`; `run_reports` now
conjures onto a non-empty cursor too. Loosening `reconciles` makes the
stand-in-wire test fail; the property test caught the echo-guard's own stale
shadow before the discard was added.

**Residual (documented):** a conjure of the stand-in's own item with the *same*
count as the shadow is indistinguishable from a legitimate move of the engine-only
item; resolved in the safe anti-laundering direction (the engine-only item wins).

## Round-5 fix (J10-fix14)

The merge/swap selection treated a different *real* item sharing a wire id as a
swap, so a disguised stack merging onto a genuine same-wire stack (pale onto
oak) lost the merged-in portion (53 of 74). Fix, in `resolve_change`:

* the reconciling branch's **merge** now fires when the slot shows the same
  item *on the wire* and either it is the same real item or the shared item is
  **stackable** (`wire_stacks`, from the engine's stack-size table). The slot
  keeps its own authoritative item and only what was added is consumed. Two
  *unstackable* look-alikes (a spear shown as a trident onto a genuine trident)
  stay a **swap**, so the N1 fix is preserved.
* `reconciles` uses the same wire test for its merge clause.
* stale doc at the old `items.rs:170` corrected ("discards the shadow", not
  "leaves the shadow alone").

Tests: `a_disguised_stack_merging_onto_a_genuine_same_wire_stack_keeps_its_count`
(74 onto 11 → slot 64, 21 remain) and
`a_disguised_stack_merging_fully_onto_a_genuine_stack_leaves_nothing_carried`;
reverting the guard fails both plus the property test. `run_reports` now seeds
genuine `oak_planks` and `trident` (wire-colliding) and merges/swaps by wire
item stackability, so it sees the merge. The menu avoids *conjuring*
wire-colliding items: a genuine conjure sharing a stand-in's wire id and a
count the stale shadow can cover is the documented indistinguishable residual.

## Round-6 fix (J10-fix15)

`wire_stacks` used the **real** item's stack size, but the client stacks by the
item it is *shown*. A horse harness (real stack 1) is shown as leather (stack
64), so the client merges it onto a leather slot; judging by the real size made
the server swap, materialising harnesses and stranding the slot's leather. Fix:
`wire_stacks` now takes `max_stack_id(engine(wire(item).id))` — the shown item's
size.

Tests: `a_stackability_changing_stand_in_still_merges` (harness 1 onto
leather 16 → leather 17, nothing carried); reverting the fix fails it and the
property test. `run_reports` seeds a `white_harness` (shown as leather) and a
genuine leather stack, and restores the `!allow_creation` full-multiset
conservation — over the **wire** multiset, since a same-wire merge converts a
disguised stack's real identity into the slot's item (the client cannot see the
difference, so only the wire view is conserved).

## Left / interface note for the J5 (J9) agent

The per-move logic keys off `compat::items::table_for(handle.codec.protocol_id())`.
If the older codecs (v766–v773) are instead routed through the modern codec
with its own per-version `Version` tables, that codec must expose the same
resolution (or so must its `protocol_id`) for the shadow to engage; today the
compat tables are reached by `protocol_id` 47 and 755–773.
