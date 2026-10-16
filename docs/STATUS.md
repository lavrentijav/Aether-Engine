# Aether Engine — Project Status

> **Read this in other languages:** [Русский 🇷🇺](STATUS.ru.md)
>
> Single source of truth for **what exists**, **what is planned**, and **what is cancelled**.
>
> Legend: ✅ done · 🚧 in progress · 📋 planned · ❄️ deferred · ❌ cancelled.
>
> Last reviewed: 2026-10-15 · Spec: v1.1
>
> Forward-looking architecture ideas not yet built (SIMD cell format refinements, 2D
> heightmap skipping, LOD/Ultra-Full render modes, a Surface API for LOD mods, Git-like
> CoW chunk storage, tiered block-property cache) live in [DESIGN_NOTES.md](DESIGN_NOTES.md)
> — don't read them as commitments until they show up here.
>
> The accepted direction for the storage/journal rework lives in
> [storage-v2/README.md](storage-v2/README.md) (journal as the single commit point,
> stores as materialized views, guarded rollback, `ItemTypeId`/instance ids). It is a
> plan, not a status report — the phases are only done when they appear below.

---

### ✅ What exists today
The project has left pure design: **Phase 0 is implemented**, several Phase 1
subsystems have landed, and there is now a runnable demo plus `aether-server`,
which a vanilla client can join from 1.8.9 through 1.21.11 and 26.1–26.3 (the
modern versions run a server-side survival game).

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
| One world, one process: opening a persistent world takes an exclusive `flock` on its directory for the process lifetime (`storage::lock`), so a second server or converter fails fast with an error naming the holder's PID instead of minting duplicate item uids and journal numbers | ✅ |
| Central key-space registry (`aether_api::keys::KEY_PREFIXES`): every top-level KV prefix and its owner in one list, with a test that refuses two subsystems sharing a prefix; the server meta keys were moved off world-reserved bytes (`P`/`V`/`K` → `D`/`F`/`O`) and still read their pre-rename keys | ✅ |
| `aether-convert`: Anvil `.mca` → KV migration (parallel, audited) | ✅ |
| `aether-baseproxy`: live vanilla → core translation (network chunk sections, block states) | ✅ |
| Basic physics: voxel AABB collision, movement + gravity (`aether-physics`) | ✅ |
| Redstone core crate (`aether-redstone`): standalone weak/strong power; wire, repeater, comparator, observer, torches, lever, button, redstone block, lamp, daylight detector, pressure plates, tripwire, target — tested, not wired to the server | ✅ (not wired) |
| Chunk generation: flat + value-noise terrain (`aether-worldgen`) | ✅ |
| Core API: `World` facade over storage/generation/physics (`aether-api`) | ✅ |
| Runnable demo: `aether` binary (worldgen + physics + storage + telemetry) with TOML config | ✅ |
| Player entity; the 1.8.9 codec and the demo read `FullBright` (always max light), while 1.21.11 and 26.x compute real light (below) | ✅ |
| Experimental join server `aether-server`, first landed as a 1.8.9 / protocol 47 preview (noise terrain, creative, full-bright); the 1.8.9 framing is verified against a raw socket client, **not yet against a live 1.8.9 client** — see the 1.21.11 and 26.x rows below | 🚧 Preview |
| Multiplayer visibility on `aether-server`: connected players spawn, move and despawn for each other, and each is added to the tab list on join (Player List Item 0x38, binary UUID + empty GameProfile properties) and removed on leave — emitted before its Spawn Player, which a 1.8 client otherwise drops | ✅ |
| Packet compression (zlib, `Set Compression`, per-connection threshold) — a spawn column drops ~45 KB → ~0.2 KB | ✅ |
| Block-change acknowledgement (1.19+ prediction sequence) — without it a client's placement predictions are never released | ✅ |
| World history (`aether-world::journal`): append-only event log, deterministic generator as the baseline, so unedited columns are never stored | ✅ |
| Rollback by player / radius / time / everything, itself recorded so it can be undone (`/rollback`) | ✅ |
| Item provenance: 64-bit uid per item instance (allocated from a block-reserving counter, never reused), ledger + duplication report (`/audit`) | ✅ |
| Item types as a 2-byte `ItemTypeId(u16)` (`aether-world::registry::items`): engine ids are vanilla's 1.21.11 ids, modded names interned from `ITEM_BEYOND`; a persisted per-world name table that **fails loud** when present but unreadable | ✅ |
| Journal event numbers are reserved in blocks (write-ahead bound, key `N`): a crash or an out-of-order append cannot reuse a number that was already saved | ✅ |
| A saved record that exists but cannot be read **refuses login** and is left untouched, instead of reading as empty and being overwritten by the first save (`server::stored`) | ✅ |
| Inspect mode (`/inspect`): hitting a block reports its history instead of changing it | ✅ |
| Filter language shared by `/lookup` and `/rollback` (`player:` `time:` `block:` `action:` `radius:`) + paged chat output (`/page`) | ✅ |
| Recovery stash: a rollback returns what was *placed* (not the world's current content), grouped by block + state tags | ✅ |
| Stash chest window on 1.21.11 — glass frame, page arrows; chat listing on versions without container support | 🚧 Preview |
| History mirror into PostgreSQL, batched off the game thread, idempotent inserts (`--features postgres`). `kind` is the body's `smallint` tag and `reason` its code; `aether_event_kinds`/`aether_destroy_reasons` are upserted from the Rust consts on connect, and `aether_events_named` joins them (plus the item name resolved from the uid's mint row) so a human query reads as text | ✅ |
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
| Vanilla decoration from the pack's placed features: ores, trees, grass and flowers, sugar cane, kelp and seagrass, lakes, springs, corals, icebergs, desert wells, dungeons, geodes, fossils, dripstone and glow lichen spreading… — vanilla-like, 99.7% of blocks against a vanilla server world, not position-exact | 🚧 Preview |
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
| **Version compat layer** (`protocol/compat`, generated by `tools/gen/compat/gen.js`): per-release block-state tables for **1.17 through 1.21.10** by name + properties, plus substitution rules so every engine block resolves to a real state | ✅ |
| **1.8.9 pre-flattening table** carries the block id in the high nibble and metadata in the low: the rules name the metadata a substituted variant lands on (wool/carpet/glass/terracotta by colour, planks/logs by wood type, flowers by species, coral by colour), so **every engine block resolves to a real 1.8 state — zero stone fallbacks** (was 422) | ✅ |
| `set_health` on join — a vanilla client tolerates its absence, every headless client stalls without it | ✅ |
| `tools/probe`: headless 1.21.11 / 26.1 client scripts for checking what the server really sends | ✅ |
| **Block states**: `BlockStateId` is the vanilla flattened state id — 29,671 states, name round-trips for every one of them | ✅ |
| Placement chooses a state from yaw, pitch, clicked face and cursor: stair/door/gate facing, slab and stair halves, log axis, button attachment, waterlogging | ✅ |
| Two-block placements (doors, tall plants) place both halves with matching properties | ✅ |
| Interaction before placement: doors, trapdoors and fence gates toggle instead of being built against; both door halves move together; sneaking still builds | ✅ |
| **Per-state collision shapes** (`registry/shapes`, generated by `tools/gen/collisions.js` from `minecraft-data` `blockCollisionShapes`): slabs are half cells, stairs stepped, fences 1.5 tall, open doors/gates passable; physics resolves an AABB against the box list and grounded entities step up a slab | ✅ |
| Stair corner shapes (`inner_left`/`outer_right`/…), transcribed from the game's own bytecode, settled at placement as vanilla does | ✅ |
| World height is `-64..=383` (448 blocks, taller than vanilla's 384); 1.17+ clients that cannot hold a negative Y are **shifted** by 64 and see the whole range as `0..=447` | ✅ |
| 1.8.9 is the one version that cannot be widened: its section bitmask is a `u16`, so 256 blocks is a protocol limit. Shifted, it shows `-64..=191` | ⚠️ Limit |
| Heightmap entry width derived from the world height instead of a literal 8 — at 384 tall it needs 9 bits, and the literal silently truncated everything above 255 | ✅ |
| 1.17 / 1.17.1 chunk sections carry a non-air count and a **4-bit indirect** palette, and an all-air section is omitted from the bitmask (the single-valued form the encoder used is a 1.18 addition a 1.17 client reads as a length). See [Known Issues A.7](KNOWN_ISSUES.md) | ✅ |
| Neighbour-dependent states for **shape** — fence/wall/pane connections, stair corners and **chest doubling** (`type=left`/`right`) — re-settle when a neighbour changes (`placement::neighbour_updates`). Redstone wire shape and door hinge still need a wider propagation pass. See [Known Issues A.6](KNOWN_ISSUES.md) | 🚧 |
| **Real lighting**: sky and block light by level-by-level mask dilation, verified against a reference flood fill; live client reads 15 in air, 14 at the water surface, 11 three metres down | ✅ |
| Uniform light sections + the protocol's empty bitsets: block light now sends **0** sections per join instead of 2,890; sky light 1,726 instead of 2,890 | ✅ |
| **Anti-xray** (`[anti_xray]`, default **off**), two modes. `engine` (cheap, default): an ore-like block with all six neighbours opaque is sent as the surrounding block (by real neighbourhood — deepslate, netherrack, andesite, …), and re-sent for real the moment a neighbour changes (break/explosion/fluid); server-side single writes (`/setblock`, creative, `/rollback`) are masked too. The hidden-block list is configurable (default every ore, ancient debris, monster spawner); per-world (`worlds`); column overhead ≈ **10 %** on a quiet machine (`cargo-locked bench -p aether-server --bench anti_xray`, target <15 %; `benches/anti_xray.rs`). `culling` (design doc): a cached per-section face graph (a flood fill over non-opaque cells, like vanilla's client `VisGraph`) plus a direction-free reachable-section BFS (margin-seeded, view-bounded), and a send-layer mask that encodes a hidden section as single-valued stone through the same per-version chunk writer; newly visible columns are re-sent nearest-first under a per-tick budget. Both wired into `players.rs::render` and the join burst; bypass is explicit only (`/visibility off`, the `aether.antixray.bypass` node granted via `[permissions] grants`, or `bypass_players`) — **operators are not bypassed automatically**, and `/visibility status` reports the state for the player; the pre-AX1 `[visibility]` table is migrated to `mode = "culling"` with a warning. See [Known Issues A.16](KNOWN_ISSUES.md) | ✅ Implemented (default off) |
| Light caching, relight on block change, and propagation across column edges — see [DESIGN_NOTES §8.10](DESIGN_NOTES.md) | ❌ |
| Player inventories: full 46 slots, per-stack identity, saved to the world store on change and restored on join (`/inv`) | ✅ |
| Economy in PostgreSQL — balances, `/pay`, listings, auction (`/sell` `/market` `/buy` `/listings` `/unlist`), operator grants (`/econ give`) | ✅ |
| Economy integrity: every trade in one transaction, `SELECT … FOR UPDATE` on listings, `CHECK (balance >= 0)` | ✅ |
| Vanilla terrain generation — noise stage bit-exact against the game (1,179,648/1,179,648 blocks); the default whenever `worldgen_data` is set, ~45 ms noise + ~170 ms per full decorated column | ✅ |
| Structure placement (`structure_set` `random_spread` + stronghold `concentric_rings`) and the template igloo (above-ground, 50 % ladder + lab, empty loot chest); villages, mineshafts and every other jigsaw/template structure and sculk are not generated. See [Known Issues A.8](KNOWN_ISSUES.md) | 🚧 |
| CI (build/test/clippy/fmt + `cargo-deny`) on a pinned toolchain + Criterion bench harness | ✅ |
| **Clients 26.1.x, 26.2 and 26.3** (protocols 775–777) with the full survival game: one modern codec parameterized by per-release tables generated from the game's own reports (`tools/gen/versions.py` — packet ids, block states, items, entity types, menus, registries, tags) plus each release's layout changes (world clocks, the `attack` packet, section fluid counts, the swing packet, byte-array bitsets). Registries go by known pack with the overworld's height overridden, or with full data to a client without the pack. Every packet is checked by decoding it with the release's own codecs (`tools/wire`) | ✅ |
| **Item version-compat layer** (`protocol::compat`, tasks J3/J10): per-release item tables generated from `minecraft-data` (`tools/gen/compat/gen_items.js` + `items.json`), a block item following its block's own substitution (`blocks.json`, via the shared resolver in `gen.js`), an item a version lacks downgraded to a stand-in shown under the real name plus an `Aether: minecraft:<id>` lore line, and a per-move lossless round trip (`resolve_change`: the destination resolves against the window's real items — cursor then slots — so a stand-in carried between slots keeps the real item and one with no source is refused, never conjured) | ✅ |
| **Behaviour-preserving stand-ins** (`protocol::compat::behaviour`, task J4): the *real* item decides the action, so a client without spears — shown a trident (a sword on 1.8.9) — still runs the spear's jab on attack and its charged lunge on release, never a trident throw; mace and wind charge classified as the second and third users | ✅ |
| **Survival gameplay on 1.21.11 and 26.1–26.3** (`game/`): 20 TPS world tick, day/night, hunger, regeneration, starvation, fall/void/lava damage, death screen, inventory dropped on death, respawn | ✅ |
| Status effects (`game/effects.rs`): Update / Remove Mob Effect on 1.21.11 and 26.1–26.3; golden/enchanted apples, poisonous potato, spider eye, pufferfish, rotten flesh and suspicious stew; regeneration, poison (never below one heart), wither, hunger, absorption, resistance, fire resistance and speed/slowness (movement-speed attribute); tick, expire, milk cures, saved with the player | ✅ |
| Mobs: cow, pig, sheep, chicken, zombie, skeleton, creeper, spider — natural spawning, melee, skeleton arrows, creeper explosions, undead burn in daylight, loot and XP; ground movement is cached A\* over the block grid (`aether_world::pathfind`, a simplified `WalkNodeEvaluator`) with a per-tick node budget, so zombies chase around walls and animals wander to reachable targets; doors and spider climbing are not implemented ([Known Issues A.10](KNOWN_ISSUES.md)) | 🚧 Preview |
| Combat: 1.9 attack cooldown, critical hits, knockback, armour reduction and wear, tool durability, PvP | ✅ |
| Item entities: block drops from loot tables, Q / drop stack, pickup with the collect animation, merging, 5-minute expiry | ✅ |
| **Vanilla loot tables** (`game/loot.rs`), read from the pack's `data/minecraft/loot_table/**.json`: pools, rolls, weighted entries, alternatives/groups/sequences, tags and nested and inline tables, the common conditions (incl. `match_tool` with silk-touch/fortune, `block_state_property`, `entity_properties` with `components`/`type_specific`) and functions (`set_count`, `apply_bonus`, `explosion_decay`, `furnace_smelt`, `enchanted_count_increase`, `limit_count`); drives block and mob drops, and fills dungeon/igloo chests on first access (an open, a hopper draw, or a break — as vanilla unpacks `LootTable`). Built-in tables are the fallback without a pack. All 1,324 pack tables parse. Two engine limits: structure chest tables are inferred, and enchant-writing functions are no-ops — see [Known Issues A.10](KNOWN_ISSUES.md) | 🚧 Preview |
| Experience orbs: ores, mob kills and smelting drop `minecraft:experience_orb` entities instead of crediting the killer; they drift to a nearby player, merge up to the orb cap, and are picked up with vanilla's ladder of orb values | ✅ |
| **Recipes from the data pack** (`game/recipes.rs`): `data/minecraft/recipe/*.json` — 1,470 recipes in 1.21.11 (706 shaped incl. genuinely mirrored, 320 shapeless matched as a bipartite assignment, 116 cooking, 254 stonecutting, 30 smithing, 32 transmutation craftable in the ordinary grid) with recursive item-tag ingredients and vanilla's five crafting remainders; 12 dynamic `crafting_special_*` listed but not run; the compiled-in tables are the no-pack fallback ([Known Issues A.10](KNOWN_ISSUES.md)) | 🚧 Preview |
| Server-authoritative windows: every click mode, 2×2 and 3×3 crafting over all 1,359 shaped + shapeless recipes, single and **double chests** (54 slots, `type=left`/`right`), furnaces, **brewing stands** and **hoppers/droppers/dispensers** persisted as block entities, smelting + fuel + stored experience released on taking the output | ✅ |
| Brewing and potions (`game/brewing.rs`): the 46-entry `minecraft:potion` registry, the vanilla `PotionBrewing` table (awkward/mundane/thick, every effect potion, redstone=longer, glowstone=stronger, fermented spider eye inversions, gunpowder=splash, dragon's breath=lingering), a brewing stand (three bottles, ingredient, blaze-powder fuel, 400-tick brew) as a window and a block entity, water bottles from a water source, drinking a potion (32 ticks, effects, glass bottle back), and thrown splash/lingering potions with distance-scaled effects and a lingering area cloud; the potion rides the `minecraft:potion_contents` component on the wire | ✅ |
| Hoppers move items on the block-tick scheduler: one every eight ticks, pull from the container above, push into the container they face (`game/blockticks/hopper.rs`); the `enabled` redstone property is ignored (always on) | 🚧 Preview |
| Survival mining: vanilla dig times, harvest tools, tool wear; placement consumes items; buckets, hoes, flint and steel, beds (spawn point, skip night), eating, bows | ✅ |
| Entity tracker: entities spawn/despawn per client by range, updates only to those tracking them | ✅ |
| Player state persisted (position, health, food, XP, mode, spawn, active status effects — save format 2, version 1 still read); gameplay commands `/gamemode /time /give /summon /tp /heal /food /killall /kill /spawn /block` | ✅ |
| Chunks stream on a per-player thread — input is read while the horizon generates | ✅ |
| **Clients 1.20.2, 1.20.3 and 1.20.4** (protocols 764–765): their own codecs, built on the 1.20.1 blob — the 1.20.2 **configuration phase** with the registry NBT in one anonymous-rooted blob, chunk-batch flow control, and 1.20.3's differences (System Chat as NBT, resource-pack UUID, game event 13, player entity-type id). They run the shared gameplay path with their pre-1.20.5 forms (NBT `Slot`, named attributes, pose 20, pre-`SpawnInfo` respawn, JSON text in 1.20.2). Encode/decode tests only; no live client of these versions was available | ✅ |
| Gameplay events on protocols 764–773 (1.20.2–1.21.10) through the shared path (`modern::play`, compat blocks and items): entities, combat, health, hunger, windows, effects. 1.8.9–1.20.1 keep the subset | ✅ |
| Water and lava flow: vanilla delays and levels, falls, slope-seeking, infinite water, lava/water → obsidian/cobblestone/stone; the `getFlow` vector carries mobs and items at the vanilla `0.014`, waterlogged blocks are contained sources, and loaded generated fluid that can flow is woken | ✅ |
| Falling blocks: sand, red sand, gravel and the sixteen concrete powders move to where they would land on their scheduled tick (no falling entity); concrete powder sets on contact with water | ✅ |
| Crop growth: wheat, carrots, potatoes, beetroot, sugar cane and cactus grow on vanilla-rate random ticks (3 per 16³ section per tick) with vanilla farmland/crowding rules; sugar cane and cactus break when their support is gone | ✅ |
| Leaf decay: a leaf recomputes `distance` to the nearest log one tick after a neighbour changes, and a non-persistent leaf at distance 7 drops its loot and disappears on a random tick | ✅ |
| Scheduled block ticks are durable: the pending falls, growth and flows are written per chunk (`BT`/`BF`, `game/tickstore.rs`) at autosave, every 30 s and on a clean stop, and reloaded as each chunk comes in — a restart no longer drops them | ✅ |
| Crop light is assumed full-bright (the engine does not simulate it), so crops grow in the dark; bone meal is not implemented | 🚧 Preview |
| Vanilla tags sent in configuration (block, item, fluid, entity type, game event — generated per release by `tools/gen/versions.py`): without the `minecraft:water` fluid tag a 1.21.11 client treats water as air (no swimming, no underwater view); `climbable` and `mineable/*` drive ladders and tool speed | ✅ |
| Journal checkpoints: a flush records, per column, which events its snapshots already hold, so loading a column replays only the events after that — not its whole history (1000 saved edits + 3 unsaved: 3 events read, was 1003) | ✅ |
| Snapshots stamped with the generator revision; one saved under an older generator is rebuilt as it loads (fresh terrain, the blocks players placed, then the journal) instead of bringing the old terrain back as a seam | ✅ |
| One client can no longer stall the server: a bounded outgoing queue and writer thread per connection, 30 s write timeout, clients silent for 30 s dropped, a repeat login replaces the old session, every departure logged with its reason | ✅ |
| Connection limits: `max_players` enforced at login (operators exempt), `max_connections` caps the connections served (`0` = `max_players*2 + 16`), `max_connections_per_ip` caps each address (`0` disables) | ✅ |
| Running as a service: clean stop on SIGINT/SIGTERM (players saved, world flushed), tick watchdog and status line, timestamped logs, `deploy/aether.service`, `AETHER_*` overrides, `tools/fetch-vanilla-data.sh`, operators bound to an address, whitelist | ✅ |
| Online mode and hybrid auth (`[auth]`): vanilla encryption handshake (server RSA-1024 keypair + AES/CFB8), Mojang `hasJoined` (uuid, name, skin properties), a durable premium-name lock store, the hybrid decision table (offline stays open to all; only names that completed online auth **here** are locked; an unlocked premium name is only *offered* auth — failure admits it offline, a mid-auth drop skips the offer for 5 min from the same address), and `/aether auth migrate <name>` | ✅ (unit-tested, not live-verified) |
| Item provenance from the survival layer (journal **format v4**): pickups, drops, chests, furnaces, crafting, eating, death drops and block drops are journalled, by diffing what a player can reach around each action. Topping up a stack is an `item_merge` (the units left a named source) rather than a move to the same slot that grew from nowhere; a `/give` mints the new units and then merges them; a split mints a new identity and moves the source; `item_derive` is reserved for a real output (craft/smelt/brew) from its consumed ingredients; a destroy carries a `reason` (consumed, despawn, crafted, creative replace, …); and `/audit` replays them with no anomalies in a full live session. v1–v3 history still decodes | ✅ |
| Grouped revisions (one revision per operation/tick) and packed, indexed history segments for the cold journal | 📋 |
| Parallel column generation + encoding on a server-wide pool of one worker per core; sections copied once per encode (cold 289-column join 20 s → 9.5 s on 4 cores, warm 1.8 s) | ✅ |
| Redstone / incremental relight / full staged physics | ❌ Not yet started |

### 📋 What is planned
Grouped by roadmap phase (see [ROADMAP.md](ROADMAP.md) for the full sequence).

**Phase 0 — Foundations**
- ✅ Cargo workspace + crate skeletons
- ✅ Runtime SIMD dispatch trait & backends
- ✅ CI (build, test, clippy, fmt, `cargo-deny`) on the pinned 1.99.0 toolchain + Criterion benches
- 🚧 Telemetry hooks (Prometheus exporter ✅; Tracy client stubbed behind a feature)

**Phase 1 — Core Engine MVP (70–75% Vanilla)**
- ✅ SoA memory model: Sub-Chunk, AVX-Cell, Morton order, masks
- ✅ Palette compression (u4/u8/u16 auto-expand) — Block-Entity arena still 📋
- 🚧 Physics pipeline (basic collision + gravity ✅; SIMD broad-phase, cached environment 📋)
- 📋 Graph-based redstone (DDG)
- 🚧 Lighting: real block/sky light by mask dilation ships on 1.21.11 and 26.x; incremental relight, caching and cross-column propagation still 📋 (the demo and 1.8.9 codec keep the `FullBright` fallback)
- 🚧 Entities + AI — server-side mobs with cached A\* pathfinding ship on 1.21.11 and 26.1–26.3; ECS storage + Flow-Field still 📋
- ✅ KV storage (Fjall + Zstd)
- 📋 Per-world process isolation + work-stealing scheduler

**Phase 2 — Hardcore Mechanical Target (≥ 95% Vanilla)**
- 📋 Cross-chunk Quasi-Connectivity redstone
- 📋 Deterministic fluids, strict update order, per-tick spawner

**Phase 3 — Network & multi-world**
- 📋 Network Gateway, Internal Binary Protocol, seamless hand-off, Vanilla protocol
- 🚧 `aether-server` as an early preview of the Vanilla-protocol layer: 1.8.9 through 1.21.11 and 26.1–26.3, survival on the modern versions

**Phase 4 — Extensibility & ops**
- 🚧 WASM plugin API: stage P1 shipped — `crates/aether-plugin` (Wasmtime 36 component model, `plugin.toml` manifest, sandbox limits, `before-*`/`after-*`/`on-tick`/`on-command` bus, `storage.kv`/`log`/`config`/`supports`) wired into `aether-server` behind the `plugins` feature, with `plugins/examples/hello` as the reference. Stage P2 (economy as a plugin, `storage.ledger`) and P3 (plugin registries) are next. The **plugin SDK (PS1)** adds an in-process **native** plugin tier (`tier = "native"`, shared service registry, load-time `requires`) and ships `plugins/sdk`: one-call helpers for permissions, commands, messages, players, config, scheduler, cooldowns and confirm prompts, callable by native and WASM dependents over one boundary crossing (`plugins/examples/greet`). The C-ABI native tier is still 📋.
- ✅ **Sidebar scoreboard (SB1)**: the native plugin `plugins/sidebar` composes a per-player panel from `plugins/sidebar/config.toml` layouts and `%placeholders%` and pushes it through a core per-player scoreboard packet path (`protocol/scoreboard.rs`) that renders 1.8 through 26.x and resends only changed lines (teams up to 1.20.2, score display names + `number_format = blank` from 1.20.3), plus the tab-list header/footer and a `sidebar@0.1` service; `%placeholders%` are a shared `sdk.placeholders@0.1` registry (menus and chat read the same one) — see [design/sidebar.md](design/sidebar.md) and [design/plugin-sdk.md §7.9](design/plugin-sdk.md).
- ✅ **Staff vanish + admin spectate (SP1)**: the native plugin `plugins/staff` drives `/vanish`, `/spectate` and `/spectate xray off|engine|culling|<level>` through core `staff_*` host hooks. A vanished player leaves the tab list, entity tracking, join/quit messages and mob targeting (vanished staff see each other); `/spectate` saves the exact state, switches to spectator (auto-vanish) and restores on `/spectate off`; the anti-xray override turns masking off/on or lowers its level for one admin and re-sends their columns; all four events are journalled — see [design/staff.md](design/staff.md), [Known Issues A.21](KNOWN_ISSUES.md).
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
