# Web admin — map + time machine (WEB2)

A read-only admin web interface, shipped as a **native plugin** (`plugins/webadmin`, tier N).

- Queue task: **WEB2** — `docs/tasks/QUEUE-WEB-admin.md`.
- Depends on **PA2** (read-only History API) and **WEB1** (HTTP server + login); neither is merged
  yet, so this stage ships the map and the time machine as self-contained modules with their own
  minimal server. See `docs/tasks/WEB2-notes.md` and `KNOWN_ISSUES §A20`.

## What it does

| piece | what |
|---|---|
| **Map** | Top-down tiles: the top non-air block of every column, coloured and height-shaded. Tiles are cached by `(zoom, tx, tz)` and **re-rendered on change** — a block edit drops every cached tile covering that column. Zoom is `2^zoom` blocks per pixel. Players/entities are an overlay layer. |
| **Time slider** | Scrub the journal: `/tile_at?…&at=<seq>` renders the world *as it was* at any past sequence number by replaying the journal over the generator baseline. `/history?x=&y=&z=` is a block's full history; `/changes?x=&z=&r=` is an area's changes over a time window. |
| **Per-player history** | `/player?id=<uuid>` returns everything a player placed, broke, took or put, filtered by time window and area; every row carries coordinates to jump the map to. |

Everything is **read-only**. No route, function or hook in this crate writes a block, an item or a
file. The time machine computes past states; it never restores them.

## Routes

Plain HTTP/1.1, one JSON payload per query. Optional shared `?token=` (or `X-Token`).

| route | returns |
|---|---|
| `GET /` | the map page (inline HTML + JS, no CDN) |
| `GET /app.js` | the page script |
| `GET /info` | `{tile_px, max_zoom, y_min, y_max, head}` |
| `GET /head` | `{head}` — the "now" sequence number |
| `GET /tile?zoom&tx&tz` | raw RGBA tile (`text/plain` blob; `X-Tile-Px` header) |
| `GET /tile_at?zoom&tx&tz&at` | the same tile replayed at sequence `at` |
| `GET /surface?x&z[&at]` | `{x,z,y,id,name,water}` — the column's top block |
| `GET /history?x&y&z` | every change to one block, oldest first |
| `GET /changes?x&z&r[&since&until]` | block changes in a radius over a time window |
| `GET /player?id=<uuid>[&since&until&x&z&r&limit]` | one player's actions |
| `GET /overlay` | live player/entity marks (empty until WEB1 supplies them) |

## Modules

| module | role |
|---|---|
| `color` | block name → RGB palette (the offline `vanilla_map` palette) with a stable fallback |
| `source` | `MapSource` / `HistorySource` — the two read-only world views, with blanket impls for `aether_api::World` |
| `map` | tile rendering, caching, invalidation |
| `timemachine` | journal replay: block history, area changes, replayed tiles |
| `playerhist` | per-player queries and human-readable summaries |
| `http` | the embedded server and the page |

The pure modules do not depend on the plugin host, so they are unit-testable against a mock and a
plain `World`; the plugin wrapper (`WebAdminPlugin`) is thin and owns only the server lifecycle.

## Wiring (when WEB1 lands)

```rust
let world = Arc::new(world);                     // aether_api::World
let admin = WebAdmin::with_config(world.clone(), world, cfg);   // one Arc, two views
let plugin = WebAdminPlugin::new(admin, "127.0.0.1:8090");
// the server's native registry adds `plugin` through `PluginHost::add_native`
```

Until then, `WebAdmin::serve(addr)` starts the server directly (used by the tests).

## Config

`WebAdminConfig`: `tile_px` (default 128), `y_min`/`y_max` (default −64…319), an optional `token`,
and `history_limit`. The plugin reads `bind` from its `[plugins.webadmin]` config.

## Tests

`cargo-locked test -p aether-webadmin-plugin` — 27 unit tests (palette, tile render/invalidate,
zoom, replay, history, routes, token, a live TCP request) plus 3 end-to-end tests against a real
`World`.
