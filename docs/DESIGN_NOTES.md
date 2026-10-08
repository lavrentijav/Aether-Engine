# Aether Engine — Forward-Looking Design Notes

> **Read this in other languages:** [Русский 🇷🇺](DESIGN_NOTES.ru.md)
>
> This is **not** a status report. Everything below is 📋 design thinking that has not
> landed in code — it exists so the direction survives between conversations and design
> sessions. When a piece of this is actually implemented, move its status line to
> [STATUS.md](STATUS.md) and its milestone to [ROADMAP.md](ROADMAP.md), and either delete
> the corresponding note here or mark it done. Check [KNOWN_ISSUES.md](KNOWN_ISSUES.md)
> before treating any gap against this document as a bug — none of this is committed yet.

---

## 1. Unified vector cell format across SIMD tiers

One geometry, not three code paths: the `AVX-Cell` stays a `4×4×2` block cell (32 blocks)
addressed as a 512-bit lane (`u16x32`).

- **AVX-512 (native):** the 512-bit lane maps straight onto hardware registers
  (`VPAVGB`, `VPERMUTEX2`, `VAND`, …) — one instruction per cell op on Zen4/Zen5-class
  and current Intel parts.
- **AVX2 (emulated fallback):** the *same* `u16x32` code, expressed through Rust's
  portable `std::simd`, is split by the compiler into two `u16x16` (256-bit) ops on
  hardware without AVX-512. No second code path to maintain; the cost is an honest
  ~2.5–3x cycle count on older cores instead of a hand-written AVX2 branch.
- Net effect: zero duplicated SIMD logic, one geometry to reason about, and a
  predictable, linear slowdown curve on older hardware rather than a cliff.

This refines the existing Phase 1 "AVX-Cell (64-byte cache line)" line in
[ROADMAP.md](ROADMAP.md) — the cell size doesn't change, this is about *how* the AVX2
fallback is produced (compiler-emulated from one source, not hand-duplicated).

## 2. 2D heightmap skipping (fast air/rock traversal)

A per-column `16×16` raster stored alongside each chunk column:

- `air_height`: the world Y above which the column is provably all-air.
- `solid_height`: the world Y below which the column is provably solid rock.

Anything outside `[solid_height, air_height]` is answered from the 2-value raster in one
comparison, without touching the `4×4×2` cell data at all. A player flying at altitude or
a ray-cast skimming over terrain resolves ~95% of a chunk's volume from two numbers
instead of walking all 24 vertical sub-chunk cells. This is the mechanism that makes long
draw distances (see §3) affordable rather than just theoretically possible.

Target: one AVX-512 lane compare per column batch. Lands naturally alongside the Phase 1
physics broad-phase and the lighting flood-fill — both already need a "is this column
worth descending into" test.

## 3. Render distance: LOD ladder vs. "Ultra-Full" mode

Two supported policies for how much geometry is honestly sent at range, both built on §2:

- **Standard (`enable_lod = true`, default):** `0–64` chunks at full `1×1×1` voxel
  detail, `64–256` chunks at LOD1 (`2×2×2`, 8x compaction), `256–512` chunks at LOD2
  (`4×4×4`, 64x compaction). ~256 B/chunk, ~400 MB/player at 512 chunks.
- **`enable_lod = false` ("Ultra-Full"):** no simplification out to 512 chunks (8 km) —
  every voxel is real, so a spyglass/optical zoom never reveals blocky LOD seams on the
  horizon. ~2 GB/player; intended for beefy private/LAN servers, not public ones. Pairs
  with a "zoom focus" mode that narrows the streamed FOV to ~10–15° while a client has an
  optical item raised, to keep the bandwidth bill sane.

This is a Phase 3 network/streaming concern — it extends the "Vanilla client protocol
compatibility layer" line in ROADMAP.md, and is independent of §4 below (§4 is for
non-Vanilla clients; the LOD ladder here is what a stock client's own chunk stream would
eventually need if draw distance grows past Vanilla norms).

## 4. Dedicated Surface/Heightmap streaming API (Voxy / Distant Horizons style)

A second, async endpoint — separate socket, separate transport — that serves *only*
surface geometry to LOD-mod clients (Voxy, Distant Horizons, or an Aether-native
equivalent):

```
[ Aether Engine Core ] --(zero-copy RAM buffer)--> [ async QUIC/UDP endpoint ] --> [ LOD-mod client ]
```

- Runs on its own socket (QUIC/UDP via Tokio), fully decoupled from the Vanilla TCP play
  connection — a slow or lossy LOD stream can never back-pressure the main game protocol.
- Payload is deliberately thin: top solid block, palette/color, and one alpha layer
  (water/leaves/glass) per column — ~512 B/chunk instead of Vanilla's full per-chunk
  block array, which is most of how "512 chunks ≈ 400 MB" instead of double digits of GB
  becomes possible.
- Pre-baked `32×32`-chunk region BLOBs handed to the NIC via `io_uring`/`sendfile` —
  no allocation or copy in the hot path, so this endpoint costs ~0 ms of tick budget: the
  main tick thread never touches it.

This is new scope for Phase 3 (Network Gateway) — not a replacement for the Vanilla
protocol compatibility layer, an addition alongside it for clients that opt in.

## 5. Git-like immutable chunk storage (instant save & rollback)

Treat chunk revisions like Git commits instead of overwriting `.mca`-style region files:

- **Immutable snapshots:** a chunk edit doesn't mutate the `4×4×2` cell in place — it
  produces a new hashed block with a parent pointer; the previous revision stays
  addressable.
- **Instant save:** "saving the world" is pinning the current root-commit hash — a
  pointer swap, not a multi-GB flush. Sub-millisecond regardless of world size.
- **Instant rollback:** undoing a grief/explosion/incident is swapping the root pointer
  back to an earlier commit — no replay, no restart, zero extra CPU.
- **Deduplication:** identical chunks (open ocean, solid stone at depth) share the same
  backing block automatically, since identity is content-addressed.

### Landed, in a different shape

The spike happened and produced something simpler than content-addressed commits, in
`aether-world::journal`. Two changes to the plan above are worth recording, because both
were surprises:

**The baseline is free, so there is nothing to dedup.** The generator is deterministic:
an untouched column can be *recomputed* more cheaply than it can be looked up. So the
world on disk is `generator(seed) + journal`, and a column nobody has edited occupies
zero bytes. That subsumes the deduplication bullet entirely — open ocean and deep stone
aren't shared, they're absent. (Test: `walking_across_an_untouched_world_writes_nothing`.)

**Undo needs the delta, not the parent chain.** Every event stores *both* sides of its
change, which makes the log invertible in place. A rollback is then a filtered backwards
walk, not a root-pointer swap — and unlike a pointer swap it can be scoped to one player
or one radius, which is what an operator actually needs after a griefing incident.
Rolling the whole world back to a moment in time is the special case, not the primitive.

What survives from the sketch: immutability (history is appended to, never rewritten — a
rollback is itself recorded and can be rolled back) and instant save (the journal is
already durable; `flush` only writes the snapshot layer that makes loading O(1) in
history length rather than O(edits)).

Item provenance came along with it: every item *instance* carries a 128-bit uid, so a
duplication shows up as one uid in two places rather than as a suspicious total.

## 6. Cell-granularity copy-on-write for zero-freeze autosave

The runtime-memory counterpart to §5: a background disk-writer thread holds an
`Arc<ChunkArray>` (read-only view). If the physics tick thread mutates a cell the writer
is currently serializing, it doesn't block — it clones just that `4×4×2` cell (CoW) and
writes the mutation there instead. Because the cell is exactly one cache line (64 B), the
clone is one `VMOVDQA32`-class copy, not a general allocation. Net effect: autosave and
the §4 streaming endpoint never stall the tick thread, and the CoW cost is a single SIMD
register move.

## 7. Tiered block property cache + range-encoded property classes

Two related ideas for `BlockStateId` / `BlockProperties` (currently
`crates/aether-world/src/block.rs`, `crates/aether-world/src/registry.rs`):

- **Range-encoded properties:** reserve contiguous sub-ranges of the id space for each
  coarse property class (opaque+collidable, transparent, semi-transparent, no-collision,
  …) so that `solid` / `transparent` / `collision` etc. are answered by a range check
  against the id itself — branchless, no registry hash lookup — instead of the current
  per-id `BlockRegistry::props_of` call. The registry would still own the *authoritative*
  properties; the ranges are a fast-path derivation, not a second source of truth.
- **Tiered cache:** a small, read-only "hot" cache holding the common vanilla block set
  (the ids that show up in nearly every cell) kept maximally cache-resident, with a
  larger "full" cache in RAM behind it for the long tail — including mod-registered
  blocks — so common cells never evict the hot set.

Both ideas extend the existing Phase 1 "Palette compression (u4/u8/u16 auto-expand)" and
"SoA masks" lines in ROADMAP.md; they're a refinement of how those masks get populated,
not a new subsystem. Needs a prototype + benchmark against the current
`BlockRegistry::props_of` path before it's adopted — the range-encoding scheme only pays
for itself if the hash lookup it replaces is actually showing up hot in profiles.

## 8. Lighting, shaped around this engine's layout

A plan, not an implementation. `FullBright` is still what runs.

The reason to write it down now: the registry gained real `light_emission` and
`light_opacity` for all 1166 vanilla blocks, so for the first time there is
something correct to propagate. The design below is not "vanilla's algorithm
ported" — it is what falls out of the four things this engine already commits
to, and in two places those commitments make a materially different algorithm
the right one.

### 8.1 The load-bearing observation

A 16³ boolean mask is `4096 bits = 512 bytes`. That is:

- **8 AVX2 registers**, or 4 AVX-512 registers;
- exactly `MASK_WORDS = 64` `u64`s, which is what `Mask` already is;
- one quarter of a client-facing 2048-byte light section.

So **the entire light frontier for a sub-chunk fits in registers**. That single
fact is what makes the approach below different from a per-cell queue, and it
is a property of this engine's SoA masks rather than of lighting.

### 8.2 Propagation as mask dilation, not as a queue

Vanilla propagates light with a BFS queue: pop a cell, look at six neighbours,
push those that brighten. Per cell, per neighbour, with a heap-allocated
frontier.

With a bitmask frontier the same computation is:

```text
for level in (1..=15).rev():
    frontier = dilate6(frontier) & !opaque
    reached[level] |= frontier
```

where `dilate6(m)` is six shifted copies of `m` OR'd together. In **linear**
order (`x + 16*z + 256*y`) those shifts are:

| direction | shift |
|---|---|
| ±x | 1 bit, masked at the 16-cell row edge |
| ±z | 16 bits |
| ±y | 256 bits = 4 whole words |

All three are word-aligned or cheap, over 64 words that are already in
registers. Fifteen levels x six directions x 64 words is on the order of six
thousand `u64` operations for a whole sub-chunk — no allocation, no pointer
chasing, no branch per cell.

Three consequences worth stating separately:

**It is deterministic by construction.** A queue's result depends on insertion
order once several sources interact; level-by-level dilation is a pure function
of the input masks. Deterministic Safe-Point merges are a design pillar here,
and this gets that for free rather than by imposing an ordering on a queue.

**It vectorises where a queue cannot.** The per-level step is data-parallel over
64 words. This is the one place in lighting where the runtime SIMD dispatch
actually earns its keep.

**Recomputing beats patching.** Vanilla maintains incremental light with a
decrease-then-increase pass, because recomputing a chunk is expensive. Here a
whole sub-chunk is a few thousand register operations, so an edit can simply
**recompute the affected sub-chunks from scratch**. That deletes the most
bug-prone code in every lighting engine ever written, and it is affordable only
because of 8.1.

### 8.3 Morton is the wrong order for this, and that is fine

The block data is Morton-ordered, which is right for neighbourhood access and
for the AVX-Cell. It is wrong here: `+1 in x` is not a shift in Morton order,
it is a magic-bits increment per index.

So the light pass works in **linear** order and transposes once per sub-chunk
when it builds its input masks. That is not a concession — the client's light
section is *also* linear (`y, z, x`), so computing in linear order removes a
transpose at send time that a Morton computation would have to pay anyway. The
transpose happens once, on the way in, instead of once per level and again on
the way out.

### 8.4 Three opacity classes, not sixteen

`light_opacity` is `0..=15`, but the distribution is not uniform: of the vanilla
set, almost every block is 0 (air, glass, plants, fences) or 15 (every full
opaque cube). The handful in between — water, ice, leaves, cobweb, slime, honey,
powder snow — are opacity 1.

So the pass carries **two masks**, not a per-cell value:

- `opaque` — opacity 15. Blocks everything. This is the existing `solid` mask,
  and `registry::blocks` now guarantees `solid == (light_opacity == 15)`, with a
  test that asserts it across all 1166 blocks precisely so this pass can rely on
  it.
- `dim` — `0 < opacity < 15`. Removed from the propagable set and re-seeded one
  level lower, which is exactly right for opacity 1 and is the only class that
  actually occurs.

An opacity of 2..14 has no vanilla instance. If a modded block ever has one, the
honest answer is to treat it as opaque and say so, rather than to complicate the
inner loop for a case that does not exist.

### 8.5 Sky light: the heightmap does most of the work

Sky light has a shape block light does not: above the terrain it is 15
everywhere, and it does **not attenuate going straight down** through
transparent blocks.

- Maintain a per-column heightmap, 16x16 `u16` = 512 bytes per chunk column.
- Every sub-chunk entirely above the column maximum is `Uniform(15)`. Every one
  entirely below the terrain with no cave is `Uniform(0)`. Neither is computed
  cell by cell; neither is stored as 2048 bytes.
- Only the band the heightmap crosses, plus anything with a cave in it, runs the
  dilation — seeded from the sky column rather than from point sources.

### 8.6 Uniform sections, and why this is really a bandwidth fix

Light is the single largest thing this server sends. A 1.21 column carries
`2 x sections x 2048` bytes of it — about 41 KB, against roughly 8 KB of blocks.
Compression already takes a spawn column from 45 KB to 183 B *because* light is
long runs of `0xFF`, which is a strong hint that the data was never worth
materialising.

```rust
enum LightSection {
    Uniform(u8),            // one byte
    Dense(Box<[u8; 2048]>), // only where it varies
}
```

Most sections in most columns are `Uniform(15)` or `Uniform(0)`. The 1.21 chunk
packet already carries BitSet masks distinguishing "empty", "all-zero" and
"present" sections, so a uniform-0 section is **not sent at all** rather than
sent and compressed away. This attacks the 41 KB at its source instead of
relying on deflate to hide it.

### 8.7 Light is derived, so it is never stored

The storage model is already `world = generator(seed) + journal`, with unedited
columns regenerated rather than read. Light is a pure function of blocks, so the
same argument applies with more force: storing it would reintroduce exactly the
bytes that model exists to avoid, and would add a second thing that can be stale.

Recompute on load, invalidate on edit. The edit signal already exists — every
block change is a journal `BlockSet` event, so a rollback relights through the
identical path with no special case.

### 8.8 Order of work

1. `LightView` implementation backed by `LightSection`, still returning 15
   everywhere. No behaviour change; makes the seam.
2. Uniform sections + the BitSet plumbing in the 1.21 chunk encoder. **This is
   the bandwidth win and it lands before any real propagation** — a world that
   is fully lit but sends `Uniform(15)` is already dramatically cheaper.
3. Heightmap, then sky light for the trivial cases.
4. Block-light dilation, scalar first, verified against a slow reference
   flood-fill written from the vanilla description — not from this
   implementation.
5. SIMD dispatch for the dilation step.
6. Incremental relight driven off journal events.

Steps 1-2 are worth doing on their own even if 4-5 never happen.

### 8.9 What landed, and what it measured

Steps 1-4 of 8.8 are implemented: `aether_world::light` holds the linear-order
[`Mask`] with its six-way dilation, the level-by-level propagation for block and
sky light, and the per-column driver that collapses uniform sections. The 1.21
chunk encoder uses the protocol's *empty* bitsets instead of sending 0xFF
everywhere.

Measured with a real 1.21.11 client joining and taking its 289-column view:

| | before | after |
|---|---|---|
| block-light sections sent | 2,890 | **0** |
| sky-light sections sent | 2,890 | 1,726 |
| light if every section were sent | 11.6 MiB | — |
| total bytes on the wire for the join | — | 175 KiB |

Block light leaves the wire **entirely** in a world with no light sources, which
is the common case for most of any world, and sky light loses 40% of its
sections to the terrain above and below. Both happen before deflate rather than
being hidden by it.

Two corrections to the plan above, both from measurement rather than thought:

**The `dim` opacity class collapses for block light.** Opacity takes exactly
three values in 1.21.11 — 0 (706 blocks), 1 (85), 15 (375) — and vanilla's cost
is `max(1, opacity)`, so opacity 1 costs what air costs. §8.4 expected two masks
on the propagation path; one is enough. The class survives only in sky light's
vertical pass, where an opacity-1 cell costs the *free* descent — which is why
the sea surface reads 15 and three metres down reads 11.

**Morton is not merely unhelpful here, it is unusable.** §8.3 said the light
pass would work in linear order. It has to: `+1 in x` in Morton is a per-index
magic-bits increment, so a frontier cannot be advanced a word at a time at all.

### 8.10 What is unverified

- **The operation counts in 8.2 are arithmetic, not measurements.** Nothing here
  has been benchmarked; "recomputing beats patching" is a claim about this
  layout that a profile could refute.
- **Sky light's vertical no-attenuation does not fit the dilation loop** and
  needs its own downward pass. That pass is a column sweep, which is the one
  access pattern neither Morton nor the AVX-Cell suits well.
- **Cross-sub-chunk propagation is not addressed above.** Light crosses the
  boundary in all six directions, so the register-resident claim holds within a
  sub-chunk but the frontier has to be exchanged at the edges — the natural fit
  is to iterate to a fixed point over the 3x3x3 neighbourhood, but the
  termination cost is unmeasured.
- **No vanilla parity claim.** Matching vanilla's light exactly is a separate
  question from computing light correctly, and it is not attempted here.
- **Light is recomputed on every chunk send and never cached.** Correct, and
  wasteful: the same column recomputes from scratch each time it enters
  someone's view.
- **A block change does not relight anything.** The `Update Light` packet is not
  sent and the column is not recomputed until it is next sent in full, so
  placing a torch lights nothing until the player reloads the chunk. This is
  step 6 of 8.8 and is the next thing worth doing.
- **Light stops at the column edge.** Propagation is per column, so a lamp does
  not light across a chunk boundary. Visible as a straight vertical seam every
  16 blocks around a light source; harmless for sky light, which is dominated by
  the vertical pass.

## 9. Block states: what an id means, and where the properties live

A design, and the contract everything downstream codes against. `BlockStateId`
has always been *documented* as a flattened block state; today it holds a block
id, which is why a stair always faces north (Known Issues A.6).

### 9.1 The decision

**`BlockStateId` is the vanilla flattened block-state id of the pinned version.**
For 1.21.11 that is `0..=29_670` — 29,671 states across 1,166 blocks.

Four reasons this particular id space and not one of our own:

- **It is fixed, complete and externally defined.** No first-touch interning, so
  the id space stops being a save-format contract that changes shape with play.
  That fragility was real: growing the seed table by one entry invalidated every
  saved world.
- **It fits what already exists.** 29,670 < 65,536, so the `u4/u8/u16` palette is
  untouched. This is a change of *meaning*, not of layout.
- **The current version's codec becomes the identity function.** `block_state()`
  for 1.21.11 stops being a table at all.
- **It is somebody else's problem to keep stable.** Mojang publishes it; we pin
  a version and regenerate.

Older codecs keep a generated `state -> their state` table, falling back to the
block's default in their version when the state has no equivalent. A 1.8.9
client shown an open trapdoor gets a closed one, not a crash.

### 9.2 Where the properties live

Three tables, and the split is chosen so the hot path is one indexed load:

```rust
STATE_PROPS:    [u16; 29_671]   // ~59 KB — packed properties, per state
STATE_TO_BLOCK: [u16; 29_671]   // ~59 KB — which block a state belongs to
BLOCKS:         [(&str, ..); 1_166]  // ~30 KB — names and per-block defaults
BY_NAME:        [(&str, u16); 1_166] // name -> block id
DEFAULTS:       [u32; 1_166]         // block id -> its default state
```

`STATE_PROPS` packs `solid | collision | redstone` into three bits and the two
light nibbles into eight, so `props_of(state)` is `STATE_PROPS[state]` and a
shift — no branch, no search, no indirection through the block.

The alternative considered and rejected: keep only per-*block* properties and a
sorted list of the states that differ from their block. It is a third of the
memory and it costs a predicated binary search on the path that physics and
lighting take for every cell. 59 KB does not fit L1 but sits comfortably in L2,
and access is highly local — a sub-chunk touches a handful of distinct states,
which is the same locality argument the palette already rests on.

`STATE_TO_BLOCK` earns its 59 KB separately: it is what gives a state its
*name*, which the journal, the rollback stash and `/inspect` all display.

### 9.3 Persistence gets simpler, not harder

The saved registry table exists because interned ids depended on play order. If
the id space is a compile-time constant tied to a pinned version, then for every
vanilla block **there is nothing to save**: id 3717 is `oak_stairs[facing=north,
half=bottom,shape=straight,waterlogged=false]` in every build that pins 1.21.11.

The table shrinks to "ids at or above 29,671", which is modded blocks only —
usually empty. The version it was pinned to is recorded alongside it, so a world
written against a different pin is refused loudly rather than reinterpreted.
That check already exists and already had to be made loud once.

### 9.4 Choosing a state on placement

Three sources of context, in increasing order of difficulty:

**(a) Already on the wire and thrown away.** `use_item_on` carries the clicked
face and the cursor position; the player's yaw is in every movement packet. That
is enough for `facing`, `axis`, `half`, `type=top/bottom`, and slab-to-double —
which covers stairs, logs, slabs, chests, doors' facing, torches, trapdoors. The
codec currently decodes these fields and discards them; `ClientEvent::Place`
needs to carry them.

**(b) One neighbour lookup.** `waterlogged` is "was the cell water", which the
world already knows at the point of placement.

**(c) A block-update pass.** Fences, walls, panes, redstone, chest doubling and
door linking are functions of *adjacent* blocks, and they have to re-run when a
neighbour changes, not only when the block is placed. This is a subsystem — a
propagation queue with a bounded radius — and it is deliberately last.

(a) and (b) are a day's work and cover what a player notices first. (c) is the
real one.

### 9.5 What this asks of world generation

The generator emits `BlockStateId`, so it now emits **states**. In practice
terrain wants very few non-default ones — `water[level=0]`,
`grass_block[snowy=false]`, `snow[layers=n]` — and the helpers below make that
explicit rather than a magic number:

```rust
blocks::default_state("minecraft:stone")                      -> BlockStateId
blocks::state("minecraft:water", &[("level", "0")])           -> Option<BlockStateId>
```

`block_ids::*` keep their names and become **default-state** ids, so existing
generator code keeps compiling and gets more correct rather than less.

### 9.6 Order of work, and what is not in it

1. Generate the four tables; `BlockStateId` becomes a state id. `block_ids::*`
   re-point. Codec `block_state()` becomes the identity for 1.21.11.
2. Placement context (a) + (b).
3. Older codecs' `state -> state` tables.
4. Block-update pass for (c).

**Not addressed:** per-state *properties* that differ from their block's. An
open door does not collide, a lit redstone lamp emits 15, a double slab is a
full opaque cube — `minecraft-data` publishes light and bounding box per block,
not per state, so the generated `STATE_PROPS` starts out uniform within a block.
That is exactly as correct as today and no more. Closing it means asking the
game itself: the unobfuscated server jar plus a small Java harness, which is the
technique the worldgen work already uses successfully.

## 10. History at scale, and the History Graph

Two threads of one design: keep the journal cheap however old the world gets, and let
an operator read it by *object* (this block, this item, this player) as well as by time.

### 10.1 The journal stays the source of truth

Not a DAG of world versions. The journal is already immutable, invertible and indexed
by column and actor; what it lacked was a bound on what a load reads and on what the
hot store holds. In order:

1. **Per-column checkpoints** — *landed* (`World::flush`, key `K`+column). A load replays
   only the events after the column's checkpoint. A sub-chunk snapshot also carries the
   generator revision it was saved under (`V`+key); a stale one is rebuilt over the fresh
   terrain, and its column then replays everything.
2. **Revisions** — group the events of one operation (a dig, an explosion, a craft, one
   tick of fluid flow) under a revision: id, parent, actor, time, cause, and the
   per-block `from → to` changes, so it stays invertible. The natural unit of a rollback
   and of a Safe-Point merge. The format leaves room for several parents, but there is
   one until regions actually branch and merge.
3. **Packed segments** — a background pass moves cold history into immutable pack files
   with their own indexes (column, actor, time, item) addressed by offset and length,
   published atomically at a Safe Point. A rollback consults the hot journal, then the
   pack indexes; nothing is unpacked wholesale. A crash mid-pass leaves the old root valid.
4. **Benchmark** at 1 / 10 / 100 M events: size, write rate, cold column load, rollback
   by player/region/time, rollback of a rollback, compaction, crash recovery.

### 10.2 The History Graph is derived

> The History Graph is a derived causal graph built from the immutable world journal.
> It relates players, events, blocks, items, containers and other world objects. It is
> not world state and does not replace the journal: lose it and it is rebuilt from the
> journal. It serves provenance queries, object-centric history, relationship analysis
> and an administrative UI.

Nodes: `Player`, `Event`/`Revision`, `Block(x,y,z)`, `Item(uid)`, `Container(x,y,z)`,
`Entity`, `Column`, open to more. Edges: *performed*, *modified*, *created / destroyed /
moved*, *stored in*, *derived from* (crafting), *caused by*, *interacted with*. Many are
virtual — read off an index rather than stored.

Indexes live in the world's own Fjall store under their own prefix, so object queries
work on every server, database or not: item uid → events, container → events,
block → events (from the column index), player ↔ player with counts and the reason.
Cluster analytics (dense subgraphs, shared containers, transfer volume) run on the
PostgreSQL history mirror where one exists. Every query takes depth, node, edge and time
limits — one busy player must not expand into half the server's history.

Analytics produce **signals with their evidence** — "A and B share chest X; diamond #51
went from A to B" — never a verdict. A dense cluster of accounts around the same
resources is something for an operator to look at, not an automatic ban.

### 10.3 The gap to close first: the journal does not record most of the graph

Checked against the code: block changes are journalled with actor, time and both sides.
Item events exist as types (`ItemMint`, `ItemMove`, `ItemDestroy`, with places inventory,
ground, container) but the survival layer barely writes them — only `/give` and the
economy do. Picking up, dropping, chests, furnaces, crafting and death drops leave no
trace; a craft has no record of its inputs, so there is no *derived from*; an event has
no cause, so an explosion is not tied to whoever lit it; fights and trades between
players are not recorded at all. A graph built today would hold blocks and little else.

### 10.4 Order of work

| Stage | What |
|---|---|
| 0 | Journal the survival layer's item flows (pickup, drop, container put/take, smelting, crafting with its inputs, death drops) and a cause on events |
| 1 | Revisions (10.1 step 2) |
| 2 | Graph indexes (item, container, player↔player) and in-game `/history block`, `/history item` |
| 3 | Read-only Admin API: object timelines, path finder between two nodes, a player's neighbours with the path that links them — loopback by default, token-protected, HTTPS through a reverse proxy |
| 4 | Web UI: dashboard (players, TPS, tick time, backlog — the status line's numbers), history explorer, node inspector |
| 5 | Cluster analytics on the PostgreSQL mirror |
| 6 | State at a moment in time, and rollback from the UI through the existing authoritative rollback (itself an event, so it appears in the graph) |

The admin UI will show addresses, movements and associations of players on a server in
offline mode, so it ships read-only and loopback-only first; a rollback from the web is a
separate, confirmed, attributed action.
