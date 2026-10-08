# Aether Engine — Known Issues & Deviations

> **Read this in other languages:** [Русский 🇷🇺](KNOWN_ISSUES.ru.md)
>
> Current problems, engineering risks, and the **Known Deviations Registry** (Phase 1
> compatibility gaps that are accepted on purpose).
>
> Last reviewed: 2026-07-28

---

### A. Current known problems (early alpha)
1. **Gameplay runs on 1.21.11 and 26.1–26.3 only; engine-level subsystems are still unbuilt.** The server now has a survival game layer (`crates/aether-server/src/game`: mobs, items, combat, hunger, crafting, chests, furnaces — see item 11), but it is a server-side layer, not the ECS/Flow-Field design of the roadmap; redstone, fluid flow and the full staged physics pipeline are not implemented.
2. **SIMD parity only partially validated.** `Scalar / SSE4.2 / AVX2` paths are implemented and unit-tested; `AVX-512` is *detected* but routed to the AVX2 path — there is no native AVX-512 backend yet.
3. **Determinism unproven.** Parallel subsystems must merge at Safe Points; the merge points are specified but not validated, and the work-stealing scheduler that would exercise them does not exist yet.
4. **Experimental 1.8.9 server is unverified against a live client.** `aether-server` speaks protocol 47 in offline mode with no compression/encryption, and its framing is checked only against a raw socket client. A real Minecraft 1.8.9 client may still reject some packets (chunk-data format is the most likely gap). It binds to loopback by default and must not be exposed publicly.
5. **Players are visible but not tab-listed.** Connected players spawn, move and despawn for each other (Spawn Player / Entity Teleport / Entity Head Look / Destroy Entities), but no Player List Item (0x38) packet is sent, so there's no tab list entry and a live client will likely render default Steve/Alex skins rather than each player's real one. Accepted for this preview — closing it means implementing the Player List Item packet's binary-UUID + GameProfile-properties encoding, which differs from Spawn Player's own string-UUID field.

6. **Block states work, except where they depend on a neighbour.** `BlockStateId` is now the vanilla flattened block-state id (29,671 states over 1,166 blocks — see [DESIGN_NOTES §9](DESIGN_NOTES.md)), and placement chooses a state from the player's yaw and pitch, the clicked face and the cursor. Re-measured against a live 1.21.11 client (`tools/probe/states3.js`):

   | placed | before | now |
   |---|---|---|
   | `oak_stairs`, looking N/E/S/W | `facing=north` in all four | follows the look direction |
   | `oak_slab` on a top face | `type=bottom` | `type=bottom` |
   | `oak_slab` on a **bottom** face | `type=bottom` | `type=top` |
   | `oak_door` | `half=lower` only, air above | two blocks, `lower` + `upper`, facing agrees |
   | placed into water | `waterlogged=false` | `waterlogged=true` |
   | two adjacent `oak_fence` | no connections | **still no connections** |

   What is left is exactly the class of property that is a function of *adjacent* blocks: fence, wall and pane connections; redstone wire shape; a chest becoming a double; a door's hinge side; grass turning `snowy`. These have to re-evaluate when a **neighbour** changes, not only when the block is placed, so they need a block-update propagation pass, which is a subsystem rather than a fix. A fence placed today is a correct, unconnected fence.

   **Interaction now beats placement.** Clicking a door, trapdoor or fence gate toggles it instead of building against it — the reported "door opens and instantly shuts with a block in front of it" was the server placing a block where the client had predicted an opening, and the ack then reverting the prediction. Both halves of a door toggle together. Sneaking still builds against it, or a door could never be placed beside another. Detected from the block's own data — anything declaring an `open` property — so there is no list to maintain.

   **Stairs take their corner shape**, transcribed from `StairBlock`'s bytecode in the 1.21.11 server rather than from memory: the four corner cases differ only in which of `left` and `right` they name. Computed at placement, which is where vanilla computes it too; vanilla *also* recomputes when a neighbour changes, and that still needs the update pass.

   **Open question, measured but not explained:** a headless client's *view* of a toggled door trails the server by a click or two, while the server's own state is right every time and nothing is placed in front. It may be the client's prediction handling rather than ours — mineflayer's differs from the game's — so this wants checking with a real client before it is chased.

   Two smaller gaps inside the part that does work:
   - **Attachment-based facing is not distinguished from look-based facing.** A ladder or a wall torch takes its direction from the surface it is on; nothing in the block's own data says so, and they get the look-based answer.
   - **Anvils are 90 degrees out.** `AnvilBlock` uses `getClockWise()`, a third rule that is not implemented.
   - **Properties do not vary within a block.** An open door still collides, an unlit redstone lamp still emits nothing, a double slab is not a full opaque cube. `minecraft-data` publishes light and bounding box per *block*; closing this means asking the game itself, which is the technique the worldgen work uses.

7. **The 1.17 / 1.17.1 chunk encoding is rejected by a real client.** A 1.17.1 client reaches the play state, reads `min_y = 0, height = 448` from our dimension type correctly, and then fails to parse the chunk data: `Attempted to read beyond the bounds of the managed data`, reading a section's block count. **Pre-existing** — measured by rebuilding with the original `height = 128` and no vertical shift, where the same client fails in the same place — so the height work did not cause it, it only made it visible. Those codecs' own documentation always said they were unverified against a live client; this is what that meant. Not chased yet.

8. **Vanilla worldgen: decoration is vanilla-like, not exact; some features and all structures are missing.** Terrain shape, biomes, surface rules (now including `temperature`, `steep`, `hole` and badlands banding) and carvers are matched stage by stage against the game's own generator. The decoration step runs the pack's placed features with the game's seeds and order, and lands 99.7% of all blocks and 96.7% of top blocks against a vanilla server world — but it is not position-exact: vanilla's result depends on which neighbouring chunks were decorated first, and trees, ores and plants overlapping a chunk border settle differently. Leaf `distance` values differ in a few percent of leaves. Not generated at all: structures (villages, mineshafts, strongholds…), geodes, dungeons, fossils, dripstone, sculk, root systems, desert wells; multiface growths (glow lichen) are placed without spreading. `minecraft:cinnabar` and `minecraft:sulfur` are not in this engine's block registry, so those surface leaves fall through rather than placing a substitute.

   ~~**Biomes are generated but not sent.**~~ **Fixed on 1.21.11:** the codec registers all 65 biomes (temperature and precipitation from `minecraft-data`, rainfall and water colours by family, vanilla's sky-colour formula) and writes each section's 64 entries as a paletted container; the other codecs and the noise generator still show plains. The block containers also widen past four bits now — a decorated section with more than sixteen states used to corrupt the column.

   **Throughput is no longer the problem.** It was: the generator once managed 35 columns in 120 s against a live client, about 10 s a column. Two bugs in the density graph's node cache did it — the `cache_once` / `cache_2d` / `flat_cache` markers were no-ops that memoized nothing, and the cell-corner cache packed `x0`/`y0`/`z0` into overlapping bit ranges, so with `min_y = -64` it collided on nearly every lookup. A third fix sized the corner cache to the real working set. Measured against a real 1.21.11 client at view radius 8: **spawn at 1.87 s, all 289 columns in ~21 s** cold, and within the first five seconds once the column cache is warm.

9. ~~**A joining client's input is not acted on until its columns have finished streaming.**~~ **Fixed:** columns now stream from a per-player thread (nearest first, re-targeted as the player moves), so the connection thread keeps reading input while the horizon generates.

10. **No headless-client regression suite yet.** `tools/probe` connects a real 1.21.11 client (mineflayer) and reports what the server actually sends; it found two live defects in one run — a seven-entry item→block table that placed every other block as **stone**, and a missing `set_health` that stalled every bot client before spawn. It is a set of scripts, not a test suite, and nothing runs it automatically. For 26.x, `tools/wire` goes further: a client compiled against the release's own server jar decodes every packet with the game's codecs and builds the registries as the client does. On its first runs it found two defects mineflayer cannot see: 26.3's byte-array `BitSet` (every chunk's light masks misread) and cave/void air counted as blocks in each section's non-air count. It needs the release's jar and Java 25, so CI does not run it either.

11. **What the survival layer does not do yet** (1.21.11 and 26.1–26.3; every older codec ignores the gameplay events and keeps its older, client-trusting subset — no mobs, no server-side windows):
    - **Mob AI is straight-line.** Mobs walk towards their goal, jump one-block steps and refuse unclimbable drops; there is no pathfinding around obstacles, no door opening, no climbing for spiders. Eight kinds exist: cow, pig, sheep, chicken, zombie, skeleton, creeper, spider. No breeding, taming, villagers or the Nether/End mobs.
    - **No redstone, crop growth, leaf decay, fire spread or falling sand/gravel.** Water and lava **do flow** now (`game/fluids.rs`: scheduled updates with vanilla delays, levels, falls, slope-seeking, infinite water, lava + water → obsidian/cobblestone/stone), but flowing water does not push mobs or items, waterlogged blocks do not flow, and generated water only starts moving when something next to it changes.
    - **No enchanting, brewing, anvils, smithing, villager trading, beds only set the spawn point and skip the night.** Status effects (golden apples, poison) are not applied; foods restore hunger only.
    - **No XP orbs.** Experience is credited to the killer directly; ores give none.
    - **Chests do not double**, and hoppers/droppers/dispensers have no inventory.
    - **Block collision is per cube**: slabs, stairs and fences collide as full blocks for mobs and item entities (as for the burial check in item 6).
    - **Anti-cheat is minimal**: mining time is checked at half the expected duration, reach at 6 blocks; movement is trusted.
    - **Light is not consulted for spawning**: monsters spawn at night under open sky, or at any time under cover.

12. **The journal keeps every event forever.** Loading a column no longer replays its whole history (per-column checkpoints), but the history itself still grows without bound on disk; grouped revisions and packed cold segments are the plan (ROADMAP, Phase 1 "History at scale"). Fluid flow is written without a journal entry, so in a sub-chunk rebuilt after a generator change, flowing water resets to what the generator made until something disturbs it.

13. **Item provenance has known blind spots.** A stack that merges into one already in a slot ends there (its uid is destroyed and the other's count grows), so that hop has no *derived from* edge; events are per click, not per operation, until revisions group them; the economy still journals its own moves with uids of their own beside the real ones; items in inventories from before this change were never minted, so their first event reads as an anomaly in `/audit`; fights and trades between players are not recorded yet.

14. **Memory at a large view radius: what is left.** Columns now leave memory and each player gets only their own view distance, so memory follows the players, not the server's age. Measured at radius 64 with one vanilla-terrain client: ~250 MiB RSS at 5 000 columns where it was ~660 MiB, and back down once the player leaves. Still open:
    - **A vanilla column is ~22 KiB, not the 10–20 hoped for.** Generated sections are genuinely mixed (stone, deepslate, ores, dirt, water, air), so most need 4-bit indices; going lower means narrower-than-4-bit encodings per run, or not keeping columns at all once sent.
    - **The generator keeps ~50 MiB of caches** (1 024 carved chunks and 1 024 chunks' decoration writes); at radius 64 the working set is larger than that, so some chunks are carved and decorated twice. A smaller cap is less memory and more CPU.
    - **The budget is in columns, not bytes.** `max_resident_columns` × ~22 KiB is the estimate; the status line shows the real figure.
    - **The client's view distance is trusted** up to the server's `view_radius`; a client claiming 64 on a 64 server gets 64.

### B. Known Deviations Registry (Phase 1 — accepted on purpose)
These are **not bugs** in Phase 1 — they are documented, temporary compatibility gaps that must be recorded in the engine config and closed in Phase 2.

| Subsystem | Phase 1 deviation | Phase 2 target |
|-----------|-------------------|----------------|
| **Redstone** | Quasi-Connectivity (QC) ignored across inactive chunk borders | Full dependency graph with cross-chunk QC |
| **Fluids** | Scheduled-update flow with vanilla delays and levels on one queue, capped per tick; no entity push, no waterlogged flow | Deterministic fluid layers 1:1 with Vanilla |
| **Update order** | Simultaneous redstone updates ordered by the parallel graph | Strict deterministic directional-priority queue |
| **Entity spawn** | Batched async spawn every N ticks (every 40 ticks, per-player caps, sky/darkness test without light levels) | Per-tick precise spawner |
| **Mob AI** | Straight-line steering with step jumps and drop avoidance | Flow-Field navigation + cached A\* |
| **Lighting** | `FullBright` fallback — every block is fully lit | Async cell-based flood-fill with safe-point merges |

### C. Engineering risks & trade-offs
- **RAM overhead: +15–25%.** SoA masks, Morton order, and redstone graphs cost more memory than classic structures. Accepted for cache-locality and SIMD wins. Sub-chunk masks are now derived from the palette on request rather than stored, which took 1.5 KiB off every resident sub-chunk.
- **x86-64 first.** The first version targets x86-64 with AVX2; other ISAs are deferred. Users on ARM/RISC-V are unsupported until later.
- **Determinism vs. parallelism.** Aggressive parallelism increases the risk of non-deterministic results if Safe-Point merges are wrong — needs strong test coverage.
- **Compatibility ceiling in Phase 1.** 70–75% Vanilla compliance means some contraptions (QC-dependent redstone, tick-perfect fluids) will behave differently until Phase 2.
- **External dependency choices.** RocksDB vs. Fjall, Elixir/OTP vs. C++/Rust for the Gateway — not finalized; a wrong pick is expensive to reverse.

### D. How to report a problem
Open a GitHub issue with: the subsystem, whether it's a **bug** or an **accepted deviation** (check the table above first), reproduction steps, and — for performance issues — a Tracy capture or the relevant benchmark scenario. See [CONTRIBUTING.md](../CONTRIBUTING.md).
