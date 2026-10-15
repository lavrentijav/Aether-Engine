# RP1 — compat continued: new blocks/items on old clients + per-version resource pack

Read `docs/tasks/COMMON.md`, `QUEUE-J-compat.md`, `QUEUE-J2-compat-wiring.md`, `docs/KNOWN_ISSUES.md` item 15,
`protocol/compat/*` (block + item tables, `compat::behaviour` stand-ins).

## Part A — continue the compat port (first)
1. Finish what item 15 lists as missing: wire the item compat layer into the pre-1.20.2 codecs; block items
   follow their block's substitution rule instead of the `barrier` placeholder; per-move round trip.
2. Audit every block/item newer than each supported release and make sure it has a stand-in that keeps
   behaviour (shape/collision/light/transparency) as close as possible; table of gaps in the doc.

## Part B — server resource pack that re-skins only the stand-ins (design first, stop for review)
Goal: a block the client lacks (say X shown as stone) must look like X, while real stone stays stone.
Technique: never use the visible block itself as the carrier. Use a **carrier block with spare block states**
that the client renders from blockstates JSON and that has no visible meaning per state:
- 1.13+: `note_block` (instrument × note × powered = 800 states), mushroom blocks / stem (6 face booleans
  = 64 states each), plus non-full carriers where needed (e.g. `tripwire` states for no-collision blocks).
- 1.8–1.12: no flattening — use unused metadata of mushroom blocks (variants 11–13 etc.) and other blocks with
  free meta; list what exists per release.
- The server keeps real instances of the carrier on one canonical state (real note blocks: sound is
  played by the server; real mushroom blocks: canonical faces), so their look never changes.
- Items: `item_model` component (1.21.4+), CustomModelData (1.14–1.21.3), damage-predicate overrides on
  tools (1.9–1.13), nothing reliable on 1.8.
- Generate one pack per pack_format from the block/item tables + vanilla textures of the newer release
  (taken from the client jar at build/deploy time, not committed — licensing), send via Resource Pack Send
  (with hash), config `[compat.pack] enabled, url/served by the server, required = false`.
- Caveats to evaluate: client-predicted mining speed (carrier hardness/tool differ from X), placement
  prediction, sounds, light emission and collision shape must match the carrier — pick per block the
  carrier with the same shape class; otherwise fall back to the plain stand-in.
Deliver `docs/design/compat-resource-pack.md` (carrier per block class per release, state budget, pitfalls),
then **stop and report**. Prototype after approval: one block (e.g. a 1.21 block on 1.20.1 via note_block).

Commit per step; EN+RU docs; build only via cargo-locked with the shared target dir.
