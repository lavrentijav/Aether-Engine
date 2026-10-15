# Compat resource pack — re-skinning only the stand-ins

Status: **approved; prototype implemented (DP1, commit `RP1-d`).** The
architecture and answers below are settled; §11's one-block prototype is in
tree. Follows `docs/tasks/QUEUE-RP-compat-pack.md` Part B and the audit in
`docs/tasks/RP1-notes.md`.

## 1. The problem

A client on release V that lacks block X sees the stand-in the compat layer
chose (J2): today usually a block of the same shape class, sometimes stone.
`pale_oak_planks → oak_planks` is fine; `copper_grate → copper_block` and
`sculk_catalyst → blackstone` are not — the client sees the wrong block, and for
the light/transparency gaps the client also culls and lights neighbours wrongly
(see the audit).

A resource pack can draw X's texture on any block the client *does* have. The
trick is to never use the visible block itself as the carrier, because then real
oak planks and disguised pale-oak planks would share a texture. Instead we use a
**carrier block with spare, invisible-by-default block states**: the server
places a specific carrier state for X, and the pack's blockstates JSON draws X
there; genuine carriers stay on a canonical state that the pack leaves alone.

## 2. Invariants

1. **Carrier identity is (block, canonical state).** For every carrier block the
   server pins real instances to one canonical state (e.g. `note_block` with
   `instrument=harp,note=0,powered=false`, mushroom blocks with all six face
   booleans `false`). The pack retextures *only the other states*, so a real
   note block or mushroom block looks exactly vanilla.
2. **A disguise is a state, not a block.** The compat mapping for X, when a pack
   is in force, is a carrier **state** (`note_block[instrument=…,note=…]`), not
   a block. `BlockTable` already stores `engine state → version state`, so this
   is a different *choice of target state*, not a new mechanism.
3. **The server is authoritative.** The carrier state exists only on the wire.
   The world stores X; every client action still decodes to X (J3/J10); the
   server never lets a client's carrier placement create a real note block.
4. **No pack → plain stand-in.** If the client declines the pack (or the
   feature is off), the codec falls back to the J2 stand-in. A carrier state is
   only ever sent when the pack is known to be active for that client.

## 3. Carrier catalogue

All numbers are state counts from `minecraft-data` (1.21.11 anchor); the
generator reads the client's own version for the release it is packing.

### 3.1 Flattened releases (1.13+)

| carrier | states | class it matches | notes |
|---|---|---|---|
| `note_block` | 500 (1.13) → 1150 (1.20.1+) | full cube, opaque, emit 0 | primary full-cube carrier; instrument×note×powered |
| `brown_mushroom_block` | 64 | full cube, opaque, emit 0 | 6 face booleans; distinct texture per face in vanilla, all spare states retextured |
| `red_mushroom_block` | 64 | full cube, opaque, emit 0 | as above |
| `mushroom_stem` | 64 | full cube, opaque, emit 0 | as above |
| `tripwire` | 128 | no collision, transparent | plants, grass, flowers, crops, water-ish, saplings |
| `vine` | 32 | no collision, transparent | second no-collision pool |
| `light` | 32 | no collision, transparent, **emit 15** | 1.17+: the only spare *emitting*, no-collision carrier |
| `redstone_lamp` | 2 | full cube, opaque-ish, emit 15 (`lit=true`) | cheap 15-light full block (see §6.2) |
| `snow` | 8 | partial height (layers 1–8) | slabs/steps-ish vertical classes |
| `candle` | 16 | small box, emit per lit state | small boxes; also a light carrier when `lit` |
| `brewing_stand` | 8 | small box, emit 1 | weak light |
| `sculk_sensor` | 96 | full cube, emit 1 | full block with weak light, many states |
| `repeater` | 64 | thin plate | 1.13+; flat non-full box |

That is roughly **1500+ free full-cube states and ~400 non-full states from
1.17 on**, against a few hundred blocks worth re-skinning per release — ample.

### 3.2 Pre-flattening releases (1.8–1.12)

No blockstates-with-free-properties: a "state" is `(block id, metadata)` and the
pack's blockstates JSON keys on metadata. There is no `note_block` by that name
(1.8/1.12 call it `noteblock`) and no instrument states; note is metadata 0–24.
The carrier pool is small and metadata-based:

| carrier | free metadata | class |
|---|---|---|
| `noteblock` | 0–24 (note) | full cube, opaque, emit 0 |
| `brown_mushroom_block` | 11–15 unused (faces use 0–10) | full cube, opaque |
| `red_mushroom_block` | 11–15 unused | full cube, opaque |
| `tripwire` | all 16 metadata | no collision, transparent |
| `vine` | 16 metadata | no collision, transparent |
| `redstone_lamp` | `lit` only 2 metadata | full cube, emit 15 |
| `glowstone`/`sea_lantern` | 1 each, emit 15 | single full-cube light slots |
| `snow` | layers 1–8 | partial height |

Budget is ~**60–100 disguises per 1.8–1.12 release**. That is enough for the
behaviour-critical subset (the light/transparency/plants gaps) but not for the
whole missing set; the rest keeps the J2 stand-in. This is the honest ceiling of
the pre-flattening era and the main reason the feature is opt-in.

## 4. Choosing a carrier per block

For each engine block X and each packed release V:

1. Read X's class from the engine registry: collision boxes (all states),
   `emitLight`, `filterLight`/`transparent`, `boundingBox`, and sound group.
2. Filter V's carriers to those whose **canonical state** has the same
   collision class, the same light emission, and the same
   transparency/occlusion. Collision and light are hard requirements (§6);
   sound and bounding-box class are preferences.
3. Take the next free state of the best carrier.
4. If no carrier matches the hard requirements — X is a slab/stair/fence/pane
   with no spare same-shape carrier, or X emits a light level V has no spare
   emitting state for — **do not use the pack for X**; keep the J2 stand-in.
5. Emit the assignment into `compat/gen/<release>.rs` (or a sibling
   `compat/pack/<format>.rs`) as another `engine state → carrier state` column,
   selected at run time only when the client has the pack.

The generator must also **reserve** the canonical states so no disguise lands on
them, and so a real carrier placed by a player is coerced back.

## 5. Items

Items cannot carry extra states, so the carrier is the item's **model**, chosen
by the client's feature level:

| era | mechanism | fit |
|---|---|---|
| 1.21.4+ | `minecraft:item_model` component → `aether:compat/<id>` | clean: any item, any look |
| 1.14–1.21.3 | `CustomModelData` (int) + model `overrides` in the pack | one int per disguise; the pack supplies the override list |
| 1.9–1.13 | damage-predicate model overrides on **damageable** items | only tools/armour; damage value selects the model |
| 1.8 | none reliable | keep the J3 plain stand-in + real display name + `Aether:` lore |

A disguised item's **display name and lore stay** as J3 sets them (the real
item's English name plus `Aether: minecraft:<id>`); the pack only changes the
picture. Behaviour is already the real item's (`compat::behaviour`).

## 6. Hard constraints (why carrier choice is picky)

These are client-side facts the pack cannot override; they must match the
carrier or the disguise breaks prediction/rendering:

1. **Collision shape** (physics + client prediction; resource packs cannot change
   it). A full-cube X must go on a full-cube carrier; a no-collision X on
   `tripwire`/`vine`/`light`; a slab on `snow`/`candle`-class or it falls back.
2. **Light emission** (client lighting; packs cannot add emission). A glowing X
   must land on an emitting carrier (`light`, `redstone_lamp:lit=true`, `candle`
   lit, `brewing_stand`, `sculk_sensor`) or fall back. This is the tightest pool.
3. **Transparency / occlusion** (neighbour culling). Grate/glass/leaf-class X
   must land on a transparent/filtered carrier.
4. **Block entity.** Only carriers with **no** block entity are safe: one adds
   a `BlockEntityData` packet, NBT and ticking. `note_block`, mushroom blocks,
   `tripwire`, `vine`, `light`, `snow`, `candle` and `redstone_lamp` are
   entity-free; `brewing_stand` has one and is listed only for its weak-light
   state. Prefer entity-free carriers; drop a carrier the moment it needs a BE.

## 7. The server keeps real carriers canonical

* On placement and on any state change, the server writes genuine carrier blocks
  as the canonical state; a redstone/right-click that would change a real note
  block's note is either suppressed or immediately canonicalised.
* A `note_block` sound is played by the server (it already knows real note
  blocks), so canonicalisation does not lose the sound; the pack leaves the
  canonical state's model and the vanilla `note_block` sound event alone.
* Mushroom blocks have no interaction; pinning their faces to `false` is enough.
* The engine's **own** light pass reads the carrier (that is what is in the
  world), so an opaque full-cube carrier is also correct for the server's
  lighting; an X that emits light is lit on the server by the *engine block* it
  really is, and only the client is fooled.

## 8. Pack generation and delivery

Pipeline (build/deploy time, not committed):

```
tools/pack/build.js --client-jar <vanilla client jar> --release <mc> \
                    --blocks <compat/gen> --out packs/compat/<pack_format>.zip
```

1. Read the pack format from the jar's `version.json`
   (`pack_version.resource`) — **never hardcode it**; the values below are for
   orientation only:
   1 (1.6–1.8.9), 2 (1.9–1.10), 3 (1.11–1.12), 4 (1.13–1.14.4), 5 (1.15–1.16.1),
   6 (1.16.2–1.16.5), 7 (1.17–1.17.1), 8 (1.18–1.18.2), 9 (1.19–1.19.2),
   12 (1.19.3), 13 (1.19.4), 15 (1.20–1.20.1), 18 (1.20.2), 22 (1.20.3–1.20.4),
   32 (1.20.5–1.20.6), 34 (1.21–1.21.1), 42 (1.21.2–1.21.3), 46 (1.21.4),
   55 (1.21.5); later releases: read from the jar.
2. Pull X's **vanilla textures** for every disguised block/item out of the jar
   (and the base item model for `item_model`). These are **not committed** —
   licensing; the build reads the operator's own jar.
3. Emit `pack.mcmeta` (`pack_format`), `assets/minecraft/blockstates/<carrier>.json`
   (variant per disguise state + the canonical variants left vanilla),
   `models/block/*`, `textures/block/*`, and item models/overrides.
4. Zip deterministically, compute SHA-1, write a manifest
   `packs/compat/index.json` mapping `pack_format → {file, sha1}`.

If the operator points the server at their own pack via the existing
`[resource_pack]`, `[compat.pack]` is off and nothing changes.

## 9. Config

```toml
[compat.pack]
# Build/serve the per-version compat packs. Off by default.
enabled = false
# Directory of generated packs (served by the operator or by the server).
dir = "packs/compat"
# Public base URL the client downloads from. Empty disables the offer.
url = ""
# Whether the client must accept. See “declined pack” below.
required = false
# Vanilla client jar used at build time only; never committed or shipped.
client_jar = ""
```

Precedence: `[compat.pack]` (generated, per-version) wins over
`[resource_pack]` (manual, one pack) for a client whose release has a generated
pack; otherwise `[resource_pack]` applies. `JoinParams.resource_pack` already
threads through every codec's Resource Pack Send, so the server only has to
select `(url, hash, required)` per client.

## 10. Pitfalls

* **Mining speed prediction.** The client predicts break time from the
  *carrier's* hardness and harvest tool (`note_block` 0.8 / axe-pref, mushroom
  0.2, tripwire instant). A stone-class X on a note-block carrier mines like
  wood on the client until the server acks. The server still owns the real
  break; it is a feel problem, not a dupe. Mitigation: prefer carriers of the
  same hardness class where the budget allows; otherwise document it (the same
  trade-off the plain stand-in already makes).
* **Placement prediction.** The client predicts placing the carrier state; the
  server converts it to X and acks. With a pack the predicted state is the
  carried *disguise* state, so the ack may move a block by one state — test the
  ack path per version (J9/T1 already gate it).
* **Sounds.** Break/step/place sound is chosen by the *carrier's* code-side sound
  group, not by the pack. Remapping is possible only globally via `sounds.json`
  and would retune real carriers too. Prefer carriers with a close sound group;
  accept the rest.
* **Declined pack (`required=false`).** A client that declines sees the raw
  carrier (note blocks / mushrooms) — visibly wrong. The server must know the
  client's status before sending carrier states:
  * 1.20.2+ answers in the configuration phase, before the first chunk.
  * Older releases answer in play, after join chunks. Until the status is known,
    send the plain J2 stand-in and re-send affected chunks on acceptance; on
    decline keep the stand-in.
  * Operators who cannot tolerate the raw-carrier window set `required=true`.
* **Multiple carriers wanted at once.** A chunk can show several distinct X on
  the same carrier; each needs its own state, so the assignment is per engine
  state, not per block, and must be stable across regenerations (hash the
  block name to the state, don't rely on iteration order).
* **Pack size / texture count.** Per release the pack embeds only textures for
  the blocks re-skinned *for that release*, not the whole registry; cap it and
  fall back to the plain stand-in past the cap.
* **Anti-xray / visibility masking** rewrites block passes; a masked section
  shows stone regardless, so carrier states never leak through it. No change
  needed, but the two features should share the per-version block writer.
* **Block entities** (§6.4): never pick a carrier that carries one.

## 11. Prototype after review (one block)

One 1.21 block on 1.20.1 via `note_block`. Steps:

1. generate a `pack_format 15` pack that retextures one spare `note_block`
   state (e.g. `instrument=…,note=1`) with that block's texture;
2. assign that engine block to that state in a `compat/pack/v1_20_1` column;
3. serve the pack via `[compat.pack]`, wait for acceptance, and send a
   `BlockChange` for a placed instance;
4. verify in-game: the disguised block looks like the 1.21 block, a real note
   block still looks and sounds vanilla, and the server holds the 1.21 block.

## 12. Decisions (from the review)

* **Every release with at least one gap** gets a pack; the generator loops
  releases rather than naming them (§8). A release with no assignments has an
  empty table and `table_for` returns `None`, so it keeps the plain stand-in.
* **`required = false` by default.** A carrier state is sent only to a player
  whose status is `successfully_loaded`; until then, on a decline, or when the
  release has no status handshake, the plain stand-in is used, and the view is
  re-sent on acceptance (§7, §10).
* **Separate `compat/pack_gen/*`, selected at run time.** `compat/gen/*` stays
  byte-identical; the pack is fully optional.
* **Checked-in assignment file** (`tools/gen/compat/pack.json`): stable,
  reviewable; the generator appends assignments and never reshuffles existing
  carrier states, so a state a client cached keeps its meaning.

## 13. Prototype in tree (commit `RP1-d`)

One block, `minecraft:pale_oak_planks`, disguised on **1.20.1** via
`note_block[instrument=harp,note=1,powered=false]` (pack_format 15):

* `compat::pack` — `PackTable`, `table_for(protocol)`, `CarrierView`; the
  carrier is an engine state (584) the codec's own `V1_20_1` table maps to the
  wire state (541) the pack retextures.
* `PlayerHandle` pack status + `render` remap, gated on `successfully_loaded`;
  every pre-1.20.2 codec reports its play-state `resource_pack_receive` id and
  the session re-sends the view on load.
* `[compat.pack]` config; `tools/gen/compat/gen_pack.js` → `pack_gen/v1_20_1.rs`;
  `tools/pack/build.js` builds the pack (merged blockstates, model, texture)
  from a client jar or a supplied texture.

Left for follow-ups (not in the prototype): auto-assignment of the full
per-release gap set; configuration-phase status for 1.20.2+; pre-flattening
(1.8–1.12) packs; item re-skinning (`item_model` / CustomModelData); and the
in-game verification, which needs a live client this environment does not have
(checked instead by tests that the wire state and the pack's variant key
agree).
