# Web admin — live ops, map + time machine (WEB1 + WEB2)

A read-only admin web interface, shipped as a **native plugin** (`plugins/webadmin`, tier N). One
embedded HTTP server (no framework, no CDN) serves both stages of the brief.

- Queue task: **WEB1** + **WEB2** — `docs/tasks/QUEUE-WEB-admin.md`.
- The server attaches the live `aether_api::World` at `install_native` and publishes the live-ops
  service (`ops@0.1`, `crates/aether-ops`) the plugin resolves for the counters. See
  `docs/webadmin.md` and `KNOWN_ISSUES §A20`.
- Auth is a static `[plugins.webadmin] token` or a five-minute one-time `/webadmin token`, carried
  as an `HttpOnly` cookie or `?token=` (or `X-Token`).

## What it does

| piece | what |
|---|---|
| **Dashboard** | TPS/MSPT (mean and slowest), players, memory, uptime, columns sent — the live-ops home. |
| **Regions** | The heaviest regions: a sampled per-region tick-cost ranking plus a heat grid. |
| **Players** | Every known player, with live/saved **inventories** and a profile (IPs seen, first/last join, playtime). |
| **Map** | Top-down tiles: the top non-air block of every column, coloured and height-shaded. Tiles are cached by `(zoom, tx, tz)` and **re-rendered on change** — a block edit drops every cached tile covering that column. Zoom is `2^zoom` blocks per pixel. Players/entities are an overlay layer. |
| **Time slider** | Scrub the journal: `/tile_at?…&at=<seq>` renders the world *as it was* at any past sequence number by replaying the journal over the generator baseline. `/history?x=&y=&z=` is a block's full history; `/changes?x=&z=&r=` is an area's changes over a time window. |
| **Per-player history** | `/player?id=<uuid>` returns everything a player placed, broke, took or put, filtered by time window and area; every row carries coordinates to jump the map to. |

Everything is **read-only**. No route, function or hook in this crate writes a block, an item or a
file. The time machine computes past states; it never restores them.

## Routes

Plain HTTP/1.1. Pages are server-rendered HTML; the map and the JSON APIs answer data. Auth is a
cookie or `?token=`/`X-Token`; `/login` is the only unauthenticated route.

| route | returns |
|---|---|
| `GET/POST /login` | the login form; a valid token sets the cookie and redirects to `/` |
| `GET /` | the **dashboard** (WEB1) |
| `GET /regions` | the heaviest-regions table and heat grid |
| `GET /players` | the players list |
| `GET /profile?id=<uuid\|name>` | one player's profile |
| `GET /inventory?id=<uuid\|name>` | one player's inventory (live or saved) |
| `GET /map` | the **map** page (inline HTML + JS, no CDN) |
| `GET /app.js` | the map page script |
| `GET /api/dashboard`, `/api/regions`, `/api/players`, `/api/player?id=`, `/api/inventory?id=` | the same data as JSON |
| `GET /info` | `{tile_px, max_zoom, y_min, y_max, head}` |
| `GET /head` | `{head}` — the "now" sequence number |
| `GET /tile?zoom&tx&tz` | raw RGBA tile (`text/plain` blob; `X-Tile-Px` header) |
| `GET /tile_at?zoom&tx&tz&at` | the same tile replayed at sequence `at` |
| `GET /surface?x&z[&at]` | `{x,z,y,id,name,water}` — the column's top block |
| `GET /history?x&y&z` | every change to one block, oldest first |
| `GET /changes?x&z&r[&since&until]` | block changes in a radius over a time window |
| `GET /player?id=<uuid>[&since&until&x&z&r&limit]` | one player's history |
| `GET /overlay` | live player/entity marks |

## Modules

| module | role |
|---|---|
| `color` | block name → RGB palette (the offline `vanilla_map` palette) with a stable fallback |
| `source` | `MapSource` / `HistorySource` — the two read-only world views, with blanket impls for `aether_api::World` |
| `map` | tile rendering, caching, invalidation |
| `timemachine` | journal replay: block history, area changes, replayed tiles |
| `playerhist` | per-player queries and human-readable summaries |
| `auth` | token auth: the config token, the one-time token, cookie parsing |
| `pages` | the live-ops HTML pages (dashboard, regions, players, profile, inventory) |
| `http` | the embedded server, the router and the map page |

The pure modules do not depend on the plugin host, so they are unit-testable against a mock and a
plain `World`; the plugin wrapper (`WebAdminPlugin`) is thin and owns only the server lifecycle.

## Wiring

The server builds the plugin with the live world and adds it in `install_native`
(`crates/aether-server/src/plugins.rs`):

```rust
let plugin = WebAdminPlugin::with_world(world);   // Arc<aether_api::World>
host.add_native(&file, &caps, &config, Box::new(plugin))?;
```

The plugin resolves `ops@0.1` from the host service registry in `init` (its manifest `requires`
it) and builds the `WebAdmin` from the attached world views. `WebAdmin::serve(addr)` starts the
server; `WebAdmin::handle(&Request)` is the pure router the tests drive without a socket.

## Config

`WebAdminConfig`: `tile_px` (default 128), `y_min`/`y_max` (default −64…319), an optional `token`,
and `history_limit`. The plugin reads `bind` and `token` from its `[plugins.webadmin]` config; the
token is seeded into the shared `Auth`, and `/webadmin token` mints a one-time one.

## Tests

`cargo-locked test -p aether-webadmin-plugin` — 40 unit tests (palette, tile render/invalidate,
zoom, replay, history, the auth/login flow, the live-ops routes and pages, a live TCP request) plus
4 end-to-end tests against a real `World` (the map/time machine and the live-ops routes through the
same router).
