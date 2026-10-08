# Aether Engine — Project Status

> **Read this in other languages:** [Русский 🇷🇺](STATUS.ru.md)
>
> Single source of truth for **what exists**, **what is planned**, and **what is cancelled**.
>
> Legend: ✅ done · 🚧 in progress · 📋 planned · ❄️ deferred · ❌ cancelled.
>
> Last reviewed: 2026-10-08 · Spec: v1.1
>
> Forward-looking architecture ideas not yet built (SIMD cell format refinements, 2D
> heightmap skipping, LOD/Ultra-Full render modes, a Surface API for LOD mods, Git-like
> CoW chunk storage, tiered block-property cache) live in [DESIGN_NOTES.md](DESIGN_NOTES.md)
> — don't read them as commitments until they show up here.

---

### ✅ What exists today
The project has left pure design: **Phase 0 is implemented**, several Phase 1
subsystems have landed, and there is now a runnable demo plus an experimental
server a vanilla 1.8.9 client can connect to.

| Item | State |
|------|-------|
| Technical specification (v1.0 & v1.1) | ✅ Written (source PDFs) |
| README (English + Russian) | ✅ |
| Roadmap, Status, Known Issues, Contributing docs | ✅ |
| License | ✅ |
| `.gitignore` for a Cargo project | ✅ |
| Cargo workspace + crate skeletons | ✅ |
| Runtime SIMD dispatch (Scalar/SSE4.2/AVX2 real, AVX-512 detected) + Morton | ✅ |
| Telemetry: counters/gauges/timers + Prometheus exporter | ✅ |
| SoA memory model: AVX-Cell, Sub-Chunk, Morton masks, palette compression | ✅ |
| KV world storage (Fjall + Zstandard blobs, order-preserving keys) | ✅ |
| `aether-convert`: Anvil `.mca` → KV migration (parallel, audited) | ✅ |
| `aether-baseproxy`: live vanilla → core translation (network chunk sections, block states) | ✅ |
| Basic physics: voxel AABB collision, movement + gravity (`aether-physics`) | ✅ |
| Chunk generation: flat + value-noise terrain (`aether-worldgen`) | ✅ |
| Core API: `World` facade over storage/generation/physics (`aether-api`) | ✅ |
| Runnable demo: `aether` binary (worldgen + physics + storage + telemetry) with TOML config | ✅ |
| Player entity + `FullBright` lighting fallback (always max light) | ✅ |
| Experimental join server `aether-server` (1.8.9 / protocol 47, noise terrain, creative, full-bright) — protocol verified against a raw socket client, **not yet against a live client** | 🚧 Preview |
| Multiplayer visibility on `aether-server`: connected players spawn, move and despawn for each other (no tab list / skins yet — see [Known Issues](KNOWN_ISSUES.md)) | ✅ |
| Packet compression (zlib, `Set Compression`, per-connection threshold) — a spawn column drops ~45 KB → ~0.2 KB | ✅ |
| Block-change acknowledgement (1.19+ prediction sequence) — without it a client's placement predictions are never released | ✅ |
| World history (`aether-world::journal`): append-only event log, deterministic generator as the baseline, so unedited columns are never stored | ✅ |
| Rollback by player / radius / time / everything, itself recorded so it can be undone (`/rollback`) | ✅ |
| Item provenance: 128-bit uid per item instance, ledger + duplication report (`/audit`) | ✅ |
| Inspect mode (`/inspect`): hitting a block reports its history instead of changing it | ✅ |
| Filter language shared by `/lookup` and `/rollback` (`player:` `time:` `block:` `action:` `radius:`) + paged chat output (`/page`) | ✅ |
| Recovery stash: a rollback returns what was *placed* (not the world's current content), grouped by block + state tags | ✅ |
| Stash chest window on 1.21.11 — glass frame, page arrows; chat listing on versions without container support | 🚧 Preview |
| History mirror into PostgreSQL, batched off the game thread, idempotent inserts (`--features postgres`) | ✅ |
| Quantity dialogue in the stash window: eight adjust buttons (±1, ±16, ±32, ±64) around the item, cancel and confirm; the ceiling is the entry's whole count, not a stack | ✅ |
| Slot counts above 64 on the wire (VarInt since 1.20.5) — a recovery manifest shows its real number | ✅ |
| Handing items over splits into stacks of 64; what will not fit becomes a `minecraft:item` entity at the player's feet and is picked back up on contact | ✅ |
| Dropped items fall through the voxel collision on a 20 Hz ticker and expire after 5 minutes (a permanent drop is an unlimited inventory) | ✅ |
| Quantities past a stack show as `(xN)` in the slot's custom name and the window title, with the badge at 1 — four digits do not fit in a badge | ✅ |
| Survival mode (`game_mode` in config): broken blocks drop to the player | ✅ |
| Slash commands from 1.19+ clients: the client sends these on their own packet, not as chat, so a chat-only decoder never saw a single one | ✅ |
| Command tree declared at join ("Declare Commands"), so the server's commands complete and colour in a real client | ✅ |
| Shape updates when a neighbour changes: fences, walls, panes and stair corners settle both ways, not only towards a block that already existed | ✅ |
| The vanilla generator serves a live join — 289 columns in ~21 s cold at view radius 8, inside five seconds once cached | ✅ |
| Vanilla surface rules interpreted from the pack, all conditions including `temperature`, `steep`, `hole` and badlands banding — 99.9998% of blocks against the game's own surface stage | ✅ |
| Vanilla carvers (caves, canyons, extra-underground caves) — matched against the game's carver stage | ✅ |
| Vanilla decoration from the pack's placed features: ores, trees, grass and flowers, sugar cane, kelp and seagrass, lakes, springs, corals, icebergs… — vanilla-like, 99.7% of blocks against a vanilla server world, not position-exact | 🚧 Preview |
| Per-section biome ids (4×4×4 quart grid) exposed on `GeneratedColumn` and kept by the column cache; the overworld biome table is built in, `biome_data` optional | ✅ |
| Biomes on the wire (1.21.11): all 65 biomes registered, per-section paletted biome containers; block containers widen past 4 bits / go direct | ✅ |
| Disk cache for unmodified generated columns, aged out after a day and swept above a cap — never a source of truth, every entry reproducible from the seed (`/cache`) | ✅ |
| A block is refused where it would be placed inside a player, their own or anyone else's | ✅ |
| Mining pays by scarcity, only for generator-placed blocks — a position the journal has never seen filled — so place-and-break is not a coin press | ✅ |
| 250 coins per 10 minutes of connected time | ✅ |
| Block registry seeds the **entire** vanilla 1.21.11 set (1166 blocks) with real properties; engine block ids are vanilla's block ids | ✅ |
| `BlockProperties` carries `light_emission` / `light_opacity`, so lighting has correct data to read | ✅ |
| 1.21.11 codec translates every engine block to its default state by table lookup (was 10 blocks and a stone fallback) | ✅ |
| Any block item resolves to its block — all 1166 placeable, verified against a live client (was: everything outside a 7-entry table placed as **stone**) | ✅ |
| `set_health` on join — a vanilla client tolerates its absence, every headless client stalls without it | ✅ |
| `tools/probe`: headless 1.21.11 / 26.1 client scripts for checking what the server really sends | ✅ |
| **Block states**: `BlockStateId` is the vanilla flattened state id — 29,671 states, name round-trips for every one of them | ✅ |
| Placement chooses a state from yaw, pitch, clicked face and cursor: stair/door/gate facing, slab and stair halves, log axis, button attachment, waterlogging | ✅ |
| Two-block placements (doors, tall plants) place both halves with matching properties | ✅ |
| Interaction before placement: doors, trapdoors and fence gates toggle instead of being built against; both door halves move together; sneaking still builds | ✅ |
| Stair corner shapes (`inner_left`/`outer_right`/…), transcribed from the game's own bytecode, settled at placement as vanilla does | ✅ |
| World height is `-64..=383` (448 blocks, taller than vanilla's 384); 1.17+ clients that cannot hold a negative Y are **shifted** by 64 and see the whole range as `0..=447` | ✅ |
| 1.8.9 is the one version that cannot be widened: its section bitmask is a `u16`, so 256 blocks is a protocol limit. Shifted, it shows `-64..=191` | ⚠️ Limit |
| Heightmap entry width derived from the world height instead of a literal 8 — at 384 tall it needs 9 bits, and the literal silently truncated everything above 255 | ✅ |
| Neighbour-dependent states — fence/wall/pane connections, redstone shape, chest doubling, door hinge — and *re-*computing a stair's shape when a neighbour changes: **need a block-update pass, not built**. See [Known Issues A.6](KNOWN_ISSUES.md) | ❌ |
| **Real lighting**: sky and block light by level-by-level mask dilation, verified against a reference flood fill; live client reads 15 in air, 14 at the water surface, 11 three metres down | ✅ |
| Uniform light sections + the protocol's empty bitsets: block light now sends **0** sections per join instead of 2,890; sky light 1,726 instead of 2,890 | ✅ |
| Light caching, relight on block change, and propagation across column edges — see [DESIGN_NOTES §8.10](DESIGN_NOTES.md) | ❌ |
| Player inventories: full 46 slots, per-stack identity, saved to the world store on change and restored on join (`/inv`) | ✅ |
| Economy in PostgreSQL — balances, `/pay`, listings, auction (`/sell` `/market` `/buy` `/listings` `/unlist`), operator grants (`/econ give`) | ✅ |
| Economy integrity: every trade in one transaction, `SELECT … FOR UPDATE` on listings, `CHECK (balance >= 0)` | ✅ |
| Vanilla terrain generation — noise stage bit-exact against the game (1,179,648/1,179,648 blocks); the default whenever `worldgen_data` is set, ~45 ms noise + ~170 ms per full decorated column | ✅ |
| Structures, geodes, dungeons, fossils, dripstone and sculk are not generated. See [Known Issues A.8](KNOWN_ISSUES.md) | ❌ |
| CI (build/test/clippy/fmt) + Criterion bench harness | ✅ |
| **Clients 26.1.x, 26.2 and 26.3** (protocols 775–777) with the full survival game: one modern codec parameterized by per-release tables generated from the game's own reports (`tools/gen/versions.py` — packet ids, block states, items, entity types, menus, registries, tags) plus each release's layout changes (world clocks, the `attack` packet, section fluid counts, the swing packet, byte-array bitsets). Registries go by known pack with the overworld's height overridden, or with full data to a client without the pack. Every packet is checked by decoding it with the release's own codecs (`tools/wire`) | ✅ |
| **Survival gameplay on 1.21.11 and 26.1–26.3** (`game/`): 20 TPS world tick, day/night, hunger, regeneration, starvation, fall/void/lava damage, death screen, inventory dropped on death, respawn | ✅ |
| Mobs: cow, pig, sheep, chicken, zombie, skeleton, creeper, spider — natural spawning, wander/flee/chase AI, melee, skeleton arrows, creeper explosions, undead burn in daylight, loot and XP | 🚧 Preview |
| Combat: 1.9 attack cooldown, critical hits, knockback, armour reduction and wear, tool durability, PvP | ✅ |
| Item entities: block drops from loot tables, Q / drop stack, pickup with the collect animation, merging, 5-minute expiry | ✅ |
| Server-authoritative windows: every click mode, 2×2 and 3×3 crafting over all 1,359 shaped + shapeless recipes, chests and furnaces persisted as block entities, smelting + fuel | ✅ |
| Survival mining: vanilla dig times, harvest tools, tool wear; placement consumes items; buckets, hoes, flint and steel, beds (spawn point, skip night), eating, bows | ✅ |
| Entity tracker: entities spawn/despawn per client by range, updates only to those tracking them | ✅ |
| Player state persisted (position, health, food, XP, mode, spawn); gameplay commands `/gamemode /time /give /summon /tp /heal /food /killall /kill /spawn /block` | ✅ |
| Chunks stream on a per-player thread — input is read while the horizon generates | ✅ |
| Gameplay events on the 18 codecs before 1.21.11 (they keep the older subset) | ❌ |
| Water and lava flow: vanilla delays and levels, falls, slope-seeking, infinite water, lava/water → obsidian/cobblestone/stone | ✅ |
| Vanilla tags sent in configuration (block, item, fluid, entity type, game event — generated per release by `tools/gen/versions.py`): without the `minecraft:water` fluid tag a 1.21.11 client treats water as air (no swimming, no underwater view); `climbable` and `mineable/*` drive ladders and tool speed | ✅ |
| Journal checkpoints: a flush records, per column, which events its snapshots already hold, so loading a column replays only the events after that — not its whole history (1000 saved edits + 3 unsaved: 3 events read, was 1003) | ✅ |
| Snapshots stamped with the generator revision; one saved under an older generator is rebuilt as it loads (fresh terrain, the blocks players placed, then the journal) instead of bringing the old terrain back as a seam | ✅ |
| One client can no longer stall the server: a bounded outgoing queue and writer thread per connection, 30 s write timeout, clients silent for 30 s dropped, a repeat login replaces the old session, every departure logged with its reason | ✅ |
| Running as a service: clean stop on SIGINT/SIGTERM (players saved, world flushed), tick watchdog and status line, timestamped logs, `deploy/aether.service`, `AETHER_*` overrides, `tools/fetch-vanilla-data.sh`, operators bound to an address, whitelist | ✅ |
| Item provenance from the survival layer: pickups, drops, chests, furnaces, crafting (output derived from its inputs), eating, death drops and block drops (with the break as cause) are journalled, by diffing what a player can reach around each action; `/audit` replays them with no anomalies in a full live session | ✅ |
| Grouped revisions (one revision per operation/tick) and packed, indexed history segments for the cold journal | 📋 |
| Parallel column generation + encoding on a server-wide pool of one worker per core; sections copied once per encode (cold 289-column join 20 s → 9.5 s on 4 cores, warm 1.8 s) | ✅ |
| Redstone / async lighting / full staged physics | ❌ Not yet started |

### 📋 What is planned
Grouped by roadmap phase (see [ROADMAP.md](ROADMAP.md) for the full sequence).

**Phase 0 — Foundations**
- ✅ Cargo workspace + crate skeletons
- ✅ Runtime SIMD dispatch trait & backends
- ✅ CI (build, test, clippy, fmt) + Criterion benches
- 🚧 Telemetry hooks (Prometheus exporter ✅; Tracy client stubbed behind a feature)

**Phase 1 — Core Engine MVP (70–75% Vanilla)**
- ✅ SoA memory model: Sub-Chunk, AVX-Cell, Morton order, masks
- ✅ Palette compression (u4/u8/u16 auto-expand) — Block-Entity arena still 📋
- 🚧 Physics pipeline (basic collision + gravity ✅; SIMD broad-phase, cached environment 📋)
- 📋 Graph-based redstone (DDG)
- 📋 Async lighting (cell flood-fill) — `FullBright` fallback ships in the meantime
- 🚧 Entities + AI — server-side mobs with straight-line AI ship on 1.21.11 and 26.1–26.3; ECS storage + Flow-Field still 📋
- ✅ KV storage (Fjall + Zstd)
- 📋 Per-world process isolation + work-stealing scheduler

**Phase 2 — Hardcore Mechanical Target (≥ 95% Vanilla)**
- 📋 Cross-chunk Quasi-Connectivity redstone
- 📋 Deterministic fluids, strict update order, per-tick spawner

**Phase 3 — Network & multi-world**
- 📋 Network Gateway, Internal Binary Protocol, seamless hand-off, Vanilla protocol
- 🚧 Experimental 1.8.9 join server (`aether-server`) as an early preview of the Vanilla-protocol layer

**Phase 4 — Extensibility & ops**
- 📋 Native C-ABI + WASM plugin API
- 🚧 Aether-Convert migration tool (initial version shipped; hardening ahead)
- 📋 In-game profiler, ops hardening

### ❌ What is cancelled / superseded
Decisions explicitly dropped or replaced during design. Kept here so the history is not lost.

| Cancelled / changed | Reason | Replaced by |
|---------------------|--------|-------------|
| **"100% Vanilla-accurate from day one"** (spec v1.0: *"priority is given to perfect Vanilla mechanics"*) | Blocked MVP on exotic edge-cases | **Progressive Enhancement** (v1.1): 70–75% in Phase 1, 95%+ in Phase 2 |
| **Per-tick precise entity spawner in Phase 1** | Too costly for the MVP tick budget | Batched async spawner in Phase 1; per-tick spawner deferred to Phase 2 |
| **Exact QC (Quasi-Connectivity) across chunk borders in Phase 1** | Requires the full cross-chunk dependency graph | Ignored in Phase 1 (registered deviation); implemented in Phase 2 |
| **OOP entity model** (`struct Player { ... }`) | Poor cache locality, blocks SIMD | Structure-of-Arrays (SoA) storage |
| **Fork of an existing Java server** (Paper/Purpur/Folia/Fabric) | Locks in Java internals; defeats the DOD/SIMD goal | Clean-room Rust engine |
| ❄️ **Non-x86 (ARM/NEON, RISC-V) targets** | Not cancelled — **deferred** until x86-64 AVX2 baseline is stable | Revisit post-Phase 1 |
