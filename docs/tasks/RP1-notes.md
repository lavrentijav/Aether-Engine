# RP1 notes — compat continued + per-version resource pack

Branch `oc/RP1`, worktree `/srv/dev/projects/Aether-Engine-wt/RP1`. Read
`docs/tasks/COMMON.md` first.

## Coordination with J10 (branch `oc/QP`)

The queue file for RP1 predates the split of J10. Two of its Part A.1 bullets —
*block items follow the block rules* and *the round trip is per move* — are
exactly what `oc/QP` is changing inside `compat::items`, `compat/item_gen/*`,
`tools/gen/compat/gen_items.js` and `session::record_slot_change`. Per the
coordinator's instruction, **this branch does not touch any of that**: it works
only in the codec files, the neutral protocol layer and the docs, and lets J10
land the block-item rules and the per-move shadow on its own.

What RP1 Part A.1 therefore does is the third bullet of item 15: bind the item
layer to the pre-1.20.2 codecs. The two J10 bullets stay open on this branch and
their item-15 sub-bullets are left to `oc/QP`.

## Part A.1 — item layer wired into the pre-1.20.2 codecs

Commit `RP1-a`.

The 1.8.9 and 1.17–1.20.1 codecs (protocols 47, 755–763) keep the
client-trusting subset: they never run the gameplay path, so they render no
arbitrary stacks and decode no creative-slot report. Three things on their join
and placement path still need the compat item layer, and `protocol/mod.rs` now
binds all three through one macro (`legacy_codec_item_hooks!`) that each old
codec invokes once:

* `initial_hotbar` returns the shared `STARTER_HOTBAR`, engine item **names**
  (stone, cobblestone, dirt, sand, gravel, glass, sandstone) that every
  supported release has under the same name — so a slot is never a stand-in and
  the server-side inventory it seeds is what placement then reads;
* `block_for_item` maps a held item name to the block it places;
* `resolve_reported_item` keeps the real engine item when a client echoes the
  stand-in back, via `legacy_item_table(protocol)` (the per-release
  `compat::items` table).

v47's hand-written 1.8 item-id hotbar (`HOTBAR: [1, 3, 2, …]`) is gone: the
`finish_join` Set Slot packets now resolve each starter name through
`V1_8.wire(..)`, so 1.8's numbering comes from the compat table like every
other release.

Tests: `protocol::tests::the_pre_1_20_2_codecs_are_wired_into_the_item_layer`
(walks all ten codecs: hotbar resolves, `block_for_item`, stand-in round trip)
and `v47::tests::the_starter_hotbar_resolves_to_this_versions_item_ids`.

Not done on purpose (see the J10 note): the full gameplay path for protocols
≤ 763. `QJ-notes.md` § J5 records that routing them through gameplay is a
`modern/` refactor, not a codec-local change, and that it cannot be verified
here without a live client. RP1's wiring is the item boundary that does not
need gameplay.

## Part A.2 — stand-in behaviour audit

Method: for the twenty supported releases, every engine block (1.21.11, 1166
blocks) that the release lacks is resolved through the same chained rules as
`tools/gen/compat/blocks.json` (legacy-only rules dropped for flattened
releases, exactly as `gen.js` does). The resolved stand-in's behaviour is
compared with the original block in the engine's own 1.21.11 data on four axes:
**light** (`emitLight`), **transparency** (`transparent`/`filterLight`),
**bounding box** (`boundingBox`) and **collision shape** (the default-state
collision boxes). Reproduce by the same script shape as `gen.js`; the resolved
tables it audits are the checked-in `compat/gen/*`.

Headline:

| | blocks |
|---|---|
| releases audited | 20 (1.8 → 1.21.9) |
| engine blocks with no same-named block in some release | 3942 substitutions |
| substitutions that keep all four axes | 2974 (75%) |
| substitutions with at least one mismatch | 968 (25%) |
| distinct (block → stand-in) mismatch pairs | 191 |
| blocks that fall back to stone | **0** |

Every engine block resolves to a real block in every release; the audit is about
*how close* the stand-in is, not about missing ones.

### Gaps by behaviour class

| class | distinct pairs | what changes | representative stand-ins | releases |
|---|---|---|---|---|
| light | 34 | an emitting block loses its glow, or a non-emitter gains one | `sculk_catalyst→blackstone` (0), `crying_obsidian→obsidian`, `magma_block→netherrack`, `soul_fire→fire`, `soul_torch→torch`, `soul_lantern→glowstone`, `lava_cauldron→cauldron`, `vault→barrel` (glow→0) | 1.8; 1.17–1.20.4 |
| transparency | 104 | opaque↔see-through (render + neighbour culling) | `copper_grate→copper_block`, `tinted_glass→glass`, `sculk_vein→vine`, `mangrove_roots→oak_log`, `pale_moss_carpet→moss_carpet`, `test_instance_block→structure_block`, `resin_clump→honeycomb_block` | 1.8; 1.17–1.21.4 |
| collision / bounding box | 155 | different collision boxes and/or a different `boundingBox` class | `*_hanging_sign→*_wall_sign` (sign box vs hanging), `copper_chain→iron_bars`, `*_shelf→bookshelf`, `decorated_pot→flower_pot`, `sniffer_egg→turtle_egg`, `frogspawn→lily_pad`, `pitcher_crop→wheat`, `mud→dirt`, `heavy_core→iron_block`, `*_copper_golem_statue→*_copper`, `dried_ghast→white_wool` | 1.8; 1.17–1.21.8 |
| light + shape (+box/transparency) | 25 | both, e.g. the 1.8 candle family | `*_candle→torch`, `campfire/soul_campfire→fire`, `end_rod→torch`, `light→barrier`, `structure_void→barrier`, `snow_block→snow` | 1.8 |

The rest of the 968 are combinations of the above (a `shape` row here, a
`transparency` row there); the four rows above partition the 191 pairs by the
axes they break.

### Interim judgement

* **Behaviour-critical and cheap to improve:** the `light` and `transparency`
  classes. A block like `copper_grate` or `tinted_glass` shown as its opaque
  parent is not just a different picture — the client culls and lights
  neighbours from it. `vault→barrel` and `sculk_catalyst→blackstone` change
  client-side light. These are candidates for tighter rules in
  `blocks.json` (a separate task; RP1 does not edit the rule files, and J10 is
  the branch that owns them).
* **Cosmetic and acceptable:** most `shape` rows (statues, shelves, chains,
  hanging signs, decorated pot). Same placement class, different silhouette;
  documented rather than chased.
* **Deliberate 1.8 cases:** `*_candle→torch`, `campfire→fire`,
  `light`/`structure_void→barrier` are the closest 1.8 has; listed so a
  reviewer can agree or overrule.

### Items

Block *items* follow the block rules (J10, `oc/QP`); on this branch the
placeholder is still in place, so the block-item part of the audit is J10's to
close. For the non-block items, applying `items.json` (with the same chaining)
leaves these residuals:

| release | non-block items with no rule | examples |
|---|---|---|
| 1.21.9 | 6 | `netherite_horse_armor`, `*_nautilus_armor` |
| 1.21.6 / 1.21.8 | 16 | copper tool/armour family |
| 1.21.1 | 22 | copper tool/armour family, `pale_oak_boat` |
| 1.20.1 | 26 | copper tools, `turtle_scute`, `wolf_armor` |
| 1.17 | 46 | chest boats, `turtle_scute`, `wolf_armor` |
| 1.8 | 208 | boats, elytra, phantom membrane, future gear |

These match J10's published count (1.21.9 is 6) and are the items whose
stand-in would need a new `items.json` rule; they are display-only until the
gameplay path reaches those codecs.

## Part B — server resource pack (design + one-block prototype)

Design: `docs/design/compat-resource-pack.md`. Approved, and the §11 prototype
is implemented in commit `RP1-d`:

* **Runtime.** `compat::pack` holds a `PackTable` (engine block state → carrier
  engine state) and `table_for(protocol)`; `CarrierView` is a `BlockSource`
  that swaps disguised states before the codec translates them. Because the
  carrier is an *engine* state chosen so the codec's own per-version block
  table lands on the matching wire state, no codec changed and `compat/gen/*`
  regenerates byte-identical.
* **Gating.** `PlayerHandle` records the client's resource-pack status; a
  carrier is sent only once the status is `successfully_loaded`. Every
  pre-1.20.2 codec reports its play-state `resource_pack_receive` id
  (`resource_pack_status_id`), the session decodes it, and on the load
  transition re-sends the view (`streamer.center(chunk, true)`). Accepted,
  declined, failed, and "never reported" all keep the plain stand-in.
* **Config.** `[compat.pack]` (`enabled`, `url`, `hash`, `required` default
  `false`, `dir`, `client_jar`). It wins over `[resource_pack]` for a release
  that has a generated pack.
* **Generator.** `tools/gen/compat/pack.json` is the checked-in, stable
  assignment file; `gen_pack.js` expands it to `compat/pack_gen/<release>.rs`
  (appends, never reshuffles). `tools/pack/build.js` builds the actual pack —
  it merges the release's own `blockstates` file with the disguised variants
  and adds a `cube_all` model plus the texture read from the operator's client
  jar (or `--texture`/`--stub-texture`); no vanilla asset is committed and
  `/packs/` is gitignored.
* **Prototype block.** `minecraft:pale_oak_planks` on 1.20.1 via
  `note_block[instrument=harp,note=1,powered=false]` (pack_format 15). Tests:
  `compat::pack::tests::*`, `players::tests::a_pack_bearing_client_sees_*`,
  `config::tests::the_compat_pack_*`,
  `protocol::tests::the_pre_1_20_2_codecs_report_*`.

Not done (documented in the design §13): auto-assigning the whole per-release
gap set; configuration-phase status for 1.20.2+; 1.8–1.12 packs; item
re-skinning; and live in-game verification (no client here — the tests assert
the carrier's wire state and the pack's variant key agree instead).
