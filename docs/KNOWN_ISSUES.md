# Aether Engine — Known Issues & Deviations

> **Read this in other languages:** [Русский 🇷🇺](KNOWN_ISSUES.ru.md)
>
> Current problems, engineering risks, and the **Known Deviations Registry** (Phase 1
> compatibility gaps that are accepted on purpose).
>
> Last reviewed: 2026-07-28

---

### A. Current known problems (early alpha)
1. **Gameplay subsystems unbuilt.** Storage, basic physics, worldgen and the core API exist, but redstone, async lighting, ECS entities/AI and the full staged physics pipeline are not implemented yet. Lighting currently uses the `FullBright` fallback (always max light).
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

8. **Four of vanilla's surface conditions are stubbed false.** The surface rules are interpreted from the pack — a column is grass over dirt over stone, deepslate below its gradient, bedrock on the floor, sand and gravel under water — but `temperature`, `steep` and `hole` always answer false, and `minecraft:bandlands` is a no-op. So snow patches, the cave-badlands variant and banded terracotta are missing. All four gate decoration, never the ground itself. `minecraft:cinnabar` and `minecraft:sulfur` are not in this engine's block registry, so those leaves fall through rather than placing a substitute.

   **Throughput is no longer the problem.** It was: the generator once managed 35 columns in 120 s against a live client, about 10 s a column. Two bugs in the density graph's node cache did it — the `cache_once` / `cache_2d` / `flat_cache` markers were no-ops that memoized nothing, and the cell-corner cache packed `x0`/`y0`/`z0` into overlapping bit ranges, so with `min_y = -64` it collided on nearly every lookup. A third fix sized the corner cache to the real working set. Measured against a real 1.21.11 client at view radius 8: **spawn at 1.87 s, all 289 columns in ~21 s** cold, and within the first five seconds once the column cache is warm.

9. **A joining client's input is not acted on until its columns have finished streaming.** Measured: a bot that joins, waits 1.5 s and then places blocks gets no block updates at all and receives every prediction ack in one burst about two seconds later — the placements are read late, after the initial stream. Waiting for the stream to finish (~21 s at view radius 8 on the vanilla generator) makes the same probe work every time. The session loop interleaves sending columns with reading the socket, so input queues behind the column stream rather than being dropped; nothing is lost, but the first half-minute of a join is unresponsive. Found with `tools/probe`, not by a test.

10. **No headless-client regression suite yet.** `tools/probe` connects a real 1.21.11 client (mineflayer) and reports what the server actually sends; it found two live defects in one run — a seven-entry item→block table that placed every other block as **stone**, and a missing `set_health` that stalled every bot client before spawn. It is a set of scripts, not a test suite, and nothing runs it automatically.

### B. Known Deviations Registry (Phase 1 — accepted on purpose)
These are **not bugs** in Phase 1 — they are documented, temporary compatibility gaps that must be recorded in the engine config and closed in Phase 2.

| Subsystem | Phase 1 deviation | Phase 2 target |
|-----------|-------------------|----------------|
| **Redstone** | Quasi-Connectivity (QC) ignored across inactive chunk borders | Full dependency graph with cross-chunk QC |
| **Fluids** | Parallel simplified spread; Java tick timing not preserved | Deterministic fluid layers 1:1 with Vanilla |
| **Update order** | Simultaneous redstone updates ordered by the parallel graph | Strict deterministic directional-priority queue |
| **Entity spawn** | Batched async spawn every N ticks | Per-tick precise spawner |
| **Lighting** | `FullBright` fallback — every block is fully lit | Async cell-based flood-fill with safe-point merges |

### C. Engineering risks & trade-offs
- **RAM overhead: +15–25%.** SoA masks, Morton order, and redstone graphs cost more memory than classic structures. Accepted for cache-locality and SIMD wins.
- **x86-64 first.** The first version targets x86-64 with AVX2; other ISAs are deferred. Users on ARM/RISC-V are unsupported until later.
- **Determinism vs. parallelism.** Aggressive parallelism increases the risk of non-deterministic results if Safe-Point merges are wrong — needs strong test coverage.
- **Compatibility ceiling in Phase 1.** 70–75% Vanilla compliance means some contraptions (QC-dependent redstone, tick-perfect fluids) will behave differently until Phase 2.
- **External dependency choices.** RocksDB vs. Fjall, Elixir/OTP vs. C++/Rust for the Gateway — not finalized; a wrong pick is expensive to reverse.

### D. How to report a problem
Open a GitHub issue with: the subsystem, whether it's a **bug** or an **accepted deviation** (check the table above first), reproduction steps, and — for performance issues — a Tracy capture or the relevant benchmark scenario. See [CONTRIBUTING.md](../CONTRIBUTING.md).
