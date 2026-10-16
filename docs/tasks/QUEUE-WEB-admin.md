# WEB — admin web interface (native plugin `plugins/webadmin`, read-only first)

Read `docs/tasks/COMMON.md`, `docs/design/plugins.md`, `docs/design/plugin-sdk.md`, DESIGN_NOTES §10 (history graph), `docs/storage-v2/README.md`, BACKLOG PA2/PM4/O3/O4.
Split into stages, each its own branch; each stage must ship working on fi2.

## PA2 — History API (prerequisite, core + SDK service `history`)
Read-only, paged journal queries: who changed block (x,y,z) and when (full per-block history), worldgen-original check, events in area×time window, item provenance chain, container access log, per-player event stream. Indexes as needed (per-column; no full scans for a single block). Capability-gated.

## WEB1 — server + live ops
- HTTP server inside the plugin on an operator-configured bind/port (default 127.0.0.1:8090), login = token from config or one-time `/webadmin token` in game; no secrets in git.
- Pages: dashboard (TPS, MSPT, players, memory), **heaviest regions** (per-chunk/region tick cost: entities, block entities, redstone/fluid updates, chunk sends — ranked table + heat overlay), players list with **inventory/ender chest/armor viewer** (live, read-only), online/offline player profile (IPs seen, first/last join, playtime).
- Needs core counters for per-chunk tick cost (cheap, sampled); expose via SDK.
## WEB2 — map + time machine
- Top-down map rendered from the world (top block colour per column, tiles cached, re-rendered on change), pan/zoom, players/entities overlay.
- **Time slider**: render any area at a past time by replaying the journal (PA2) — scrub through history, see what changed and who did it; click a block → its full history.
- Per-player history: everything a player placed/broke/took/put, filterable by time/area; jump to map.
## WEB3 — relations / investigations
Graph between players by: shared IPs, item transfers (drops picked up, containers put/took, trades), shared places (same area/time, co-presence), shared containers (both accessed the same chest), market sales (when the market/economy exists — design the edge type now, populate later). Click-through to evidence (journal events). Alt-account detection score.
## Rules
Read-only (no world writes from web in these stages). Plain HTML+small JS, no heavy frameworks, no external CDNs at runtime (served from the plugin). Tests per stage; docs EN+RU.
