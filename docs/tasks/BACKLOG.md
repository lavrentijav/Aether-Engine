# Backlog — prepared, not queued (2026-10-14)

Sources: `docs/ROADMAP.md` (📋/🚧), `docs/STATUS.md` (❌/🚧), `docs/KNOWN_ISSUES.md` (A.10–A.15),
`docs/storage-v2/README.md` (phases 2, 4–7). Excludes work already running or queued: S1–S4, T1–T5,
E1, R (redstone stage 1.x), P1–P3, U1–U2.

Size: S ≈ one agent-session, M ≈ 2–4 commits, L = split before giving out.
Owner: **oc** = opencode agent, **C** = Claude (architecture/invariants), **oc+C** = agent implements, Claude designs/reviews.
Each item becomes a self-contained brief in `docs/tasks/` when it is queued.

## 0. Plugin system first (rule: optional, non-hot-path features are plugins — CONTRIBUTING "Core or plugin?")

The plugin host is the base for a whole class of features, so it goes first. P1 (host, events) is
running; P2 (economy service) and P3 (registries) follow. These extend the API so the existing
optional built-ins can leave the core:

| ID | Task | Needs | Size | Owner |
|---|---|---|---|---|
| PA1 | **UI API for plugins**: container windows (chest-style menus with click callbacks, paging), titles/actionbar/bossbar/scoreboard/sidebar, sign/anvil text input, through the compat layer for every version | P1 | M | oc+C |
| PA2 | **History API**: read-only journal queries for plugins (who changed this block, is it worldgen-original, item provenance, events in an area/time), paged, behind a capability | P1 | M | oc+C |
| PA3 | **Net capability**: outbound HTTP(S) client for plugins (allow-list per plugin in operator config), timeouts, no blocking the tick (async result via scheduler callback) | P1 | S | oc |
| PM1 | Migrate **economy** to `plugins/economy` (= P2) and delete the built-in after the conformance suite passes | P2 | M | oc |
| PM2 | Migrate **rewards** (mining/playtime coins; uses PA2 "worldgen-original" lookup and the economy service) to `plugins/rewards` | PA2, PM1 | S | oc |
| PM3 | Migrate **`/stash`** UI (rollback recovery container) to `plugins/stash`; the core keeps the rollback and the recovered-items store, the plugin shows/claims them via PA1/PA2 | PA1, PA2 | M | oc |
| PM4 | **Web admin** (O6) as a plugin with PA2 + an HTTP server capability (operator-granted port) | PA2 | M | oc |
| PM5 | Example/dogfood plugins: kits, `/home` + `/tpa`, chat formatting/moderation, a shop UI over the economy service | PA1, PM1 | M | oc |

Core stays core (hot path or vanilla mechanics): G1–G10 gameplay, redstone, light, protocol (N1–N4),
storage (H2–H9), performance (O1–O5). Where they need customisation they expose events/registries.
H6 (ClickHouse exporter) may become a plugin once PA2 exposes segment export; decide at H2.

## 1. Gameplay

| ID | Task | Needs | Size | Owner |
|---|---|---|---|---|
| G1 | **Mob behaviour pack**: breeding (food, love mode, baby growth), taming wolf/cat/parrot, sitting/following; spider climbing; zombies break/open wooden doors on hard; uses S3 pathfinding | S3 | M | oc |
| G2 | **More mobs**: enderman (teleport, block carry), witch (potions — S2), slime (split), drowned, husk/stray variants, bat, squid/fish basic; data-pack loot (U1) | S2, S3, U1 | L→split | oc |
| G3 | **Villagers & trading**: professions from POI blocks, trade tables (data pack `trade` / hard-coded vanilla tables), restock, levels, gossip-lite; trading window; iron golem spawn later | S3, U1, T2 | L→split | oc+C |
| G4 | **Fire**: fire block spread/burnout with vanilla rates, flammability table, lava ignition, fire aspect/flint & steel/fire charge, rain extinguish | — | M | oc |
| G5 | **Growth**: bone meal (crops, saplings, grass, flowers), saplings grow into trees **using the worldgen tree features** (same code path as decoration), mushrooms, sugar cane/cactus already; crops consult light once S4 lands | S4 | M | oc |
| G6 | **Light-based spawning**: monster spawn rules with block/sky light (vanilla `isDarkEnoughToSpawn`), spawner blocks (monster rooms) spawning their mob | S4 | S | oc |
| G7 | **Smithing table**: netherite upgrade, armor trims (templates, materials) as item components | T2 | S | oc |
| G8 | **Movement anti-cheat**: server-side movement validation using `aether-physics` (speed, fly, noclip, step, fall damage trust), with tolerances and rubber-banding; config levels | — | M | oc+C |
| G9 | **Nether & End**: dimension registry, nether/end noise generation from the data pack (same density engine as the overworld), portals (nether frame detection, end portal/gateway), dimension change packets, per-dimension storage | — | L→split | oc+C |
| G10 | **Jigsaw structures**: villages, pillager outposts, ancient cities, trail ruins, mineshafts (jigsaw/template pools from the pack, built on F placement + template loader) | F (merged) | L→split | oc |

## 2. Redstone (after stage 1.x is declared STABLE by review)

| ID | Task | Needs | Size | Owner |
|---|---|---|---|---|
| R-A | Comparator (compare/subtract, container fill level — containers exist), observer, daylight detector, pressure plates, tripwire, target block | R stable | M | oc |
| R-B | Pistons & sticky pistons (push limit 12, movable/immovable table, slime/honey later), **quasi-connectivity** | R stable | M | oc |
| R-C | Consumers: doors/trapdoors/fence gates react to power, rails (powered/detector/activator), dispensers/droppers fire (behaviours), hopper `enabled`, note block, TNT priming | R-A | M | oc |
| R-D | **Wire into the server**: `RedstoneView` over the world, scheduled ticks on the game scheduler (durable via tickstore), journal actor, client updates; perf budget per tick | R-A | M | oc+C |
| R-E | Phase-2 target: dependency-graph compiler for large circuits (deterministic ordering) — design first | R-D | L | C |

## 3. Protocol / compatibility

| ID | Task | Needs | Size | Owner |
|---|---|---|---|---|
| N1 | **Headless-client regression suite** (KNOWN_ISSUES A.9): extend `tools/probe` (mineflayer) to every supported version: join, walk, break/place, open chest, eat, take damage; run in CI nightly or as a script | — | M | oc |
| N2 | **Online mode & transport**: verify and complete Mojang session auth, encryption, compression thresholds, keep-alive/timeout, proxy forwarding (Velocity modern forwarding) | — | M | oc+C |
| N3 | 1.8.9 gameplay path (the last codec without it) — only if 1.8.9 support is still wanted | T4 | L | oc |
| N4 | Per-player view distance & chunk send rate limiting (bandwidth budget per client), works with S1 | S1 | S | oc |

## 4. Storage / history (storage-v2 phases)

| ID | Task | Needs | Size | Owner |
|---|---|---|---|---|
| H2 | **Journal v2**: batched segments with CRC, group fsync off the tick, `journal.level` switch, retention by days | — | L | **C** (format) + oc (tests/tooling) |
| H4 | Cross-domain atomic ops (block ↔ inventory in one record) + idempotent replay on startup | H2 | L | **C** |
| H5 | Guarded rollback (CAS on post-image), conflict report, tainted policy | H4 | M | **C** |
| H6 | ClickHouse exporter (segments → remote server, INSERT-only user, dedup token = segment id) + verification + `/history` cold queries | H2 | M | oc + C review |
| H7 | Causal `ActionId`, worldgen-derived ids, census/dupe detectors | H4 | M | later |
| H8 | **Backups**: consistent snapshot of the KV store + journal position, restore tool, retention; `/aether backup` | — | S | oc |
| H9 | **Block-entity arena** (ROADMAP Phase 1): unified storage for chests/furnaces/signs/banners/spawners instead of per-system meta keys | — | M | oc+C |

## 5. Performance / ops

| ID | Task | Needs | Size | Owner |
|---|---|---|---|---|
| O1 | Benchmark stress fixtures (the four QA fixtures from ROADMAP) + **CI benchmark gates** (regression > X% fails) | — | M | oc |
| O2 | Work-stealing scheduler: generation, save, lighting off the main tick (rayon/crossbeam), with Safe-Point merge | S4 | L | oc+C |
| O3 | Grafana dashboards + full Prometheus metrics (tick time, per-system time, chunk sends, journal lag, plugin time) | — | S | oc |
| O4 | `/aether profile`: sub-tick profiler (per system, per plugin), output to chat/web | P1 | M | oc |
| O5 | Physics: staged pipeline, SIMD broad-phase, `Cached Environment` fast path (ROADMAP 🚧) | — | M | oc |
| O6 | Web admin → moved to **PM4** (plugin) | — | — | — |

## 6. Plugins (after P1–P3)

| ID | Task | Needs | Size | Owner |
|---|---|---|---|---|
| PL4 | Native C-ABI plugin tier on the same WIT world | P3 | M | oc+C |
| PL6 | Plugin hot reload (drain events, swap instance, keep KV) | P3 | M | oc |

## Suggested order

0. **Plugins first**: P1 (running) → P2/PM1 → PA1, PA2, PA3 in parallel → PM2, PM3, PM4, PM5.
1. Now-ish, independent: **N1** (regression suite pays for everything else), **G4**, **H8**, **O3**, **N2**.
2. After the running wave lands (S3/S4/T2/U1): **G1**, **G5**, **G6**, **G7**, **N4**.
3. After redstone is STABLE: **R-A → R-B / R-C → R-D**.
4. Claude in parallel: **H2 → H4 → H5**, then hand **H6** to an agent.
5. Big ones to split with a design first: **G3**, **G9**, **G10**, **O2**.
