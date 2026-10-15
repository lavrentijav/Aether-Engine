# Web admin (native plugin `plugins/webadmin`)

> **Read this in other languages:** [Русский 🇷🇺](webadmin.ru.md)
>
> Stages **WEB1** (live ops) and **WEB2** (map + journal time machine) of
> [`tasks/QUEUE-WEB-admin.md`](tasks/QUEUE-WEB-admin.md): one read-only HTTP server inside the
> plugin. Relations are WEB3.

The web admin is a **native plugin** (tier N): compiled into `aether-server`, enabled and disabled
like any other plugin, and linked in-process so it can read live game state without a serialization
boundary. It writes nothing — there is no world write from the web in these stages. One HTTP router
serves the live-ops pages (dashboard, regions, players/inventories) and the map/time-machine pages;
the plugin README (`plugins/webadmin/README.md`) has the WEB2 detail.

## Architecture

```
aether-server ── publishes ──▶ plugin host service registry ──▶ plugins/webadmin
   │  `ops@0.1` (ServerOps, crates/aether-ops trait)                 │   one HTTP router:
   │  TPS/MSPT · memory · players · regions · profiles/inventories   │   live ops + map + time
   │                                                                 │   plain HTML + small JS
   ▼  also attaches the live World (install_native) ────────────────▶  map + journal replay
 game tick ── samples ──▶ crates/aether-server/src/web.rs
```

* `crates/aether-ops` — the **one** shared crate: the `OpsService` trait and its serialisable
  views (`Dashboard`, `RegionStat`, `PlayerEntry`, `PlayerProfile`, `InventoryView`). The server
  implements it; the plugin reads it. Neither names the other's internals, and the core builds and
  runs with the plugin absent.
* `crates/aether-server/src/web.rs` — the server half: tick-window metrics, the region sampler, the
  per-player profile store and the `OpsService` implementation. `plugins.rs` registers it as
  `ops@0.1` **before** the native plugins load, so the plugin's `requires = ["ops@0.1"]` resolves.
* `plugins/webadmin` — the HTTP server, token auth and pages. No framework, no external CDN.

## Enabling it

Plugins are off when `[plugins] dir` is empty. Set it, then enable the web admin:

```toml
[plugins]
dir = "plugins"                 # plugin discovery must be on (as for any plugin)

[plugins.webadmin]
enabled = true
bind = "127.0.0.1:8090"         # default; loopback only
token = "change-me"             # optional; omit to use `/webadmin token`
```

No secret is compiled in. If `token` is omitted (or empty), the only way in is the in-game command
`/webadmin token` (operator level 3), which mints a **one-time** token valid for five minutes and
prints it; issuing a new one invalidates the old. The token reaches the browser as an `HttpOnly`
cookie after the login form, or directly as `?token=` (handy for `curl`).

## Pages

The plugin is the single HTTP router for both WEB1 (live ops) and WEB2 (map + time machine).

| Path | What it shows |
|------|---------------|
| `/login` (GET/POST) | The token login form; a valid token sets the cookie and redirects to `/`. |
| `/` | **Dashboard**: TPS, MSPT (mean and slowest), players online/max, memory, uptime, ticks, columns sent. Auto-refreshes every 2 s from `/api/…`. |
| `/regions` | **Heaviest regions**: a ranked table and a coloured heat grid, one cell per 32×32-chunk region. |
| `/players` | Every known player: state, address, first/last seen, playtime. |
| `/profile?id=<uuid\|name>` | Profile: first/last seen, playtime, addresses seen, and live position/health/food/mode when online. |
| `/inventory?id=<uuid\|name>` | **Inventory viewer**: live when online, from the saved store when offline; main inventory + hotbar + offhand, armour, the held slot. |
| `/api/dashboard`, `/api/regions`, `/api/players`, `/api/player?id=`, `/api/inventory?id=` | The same data as JSON. |
| `/map` | **Map**: top-down tiles, pan/zoom, a live player/entity overlay, and a journal time slider (WEB2). |

### Map and time machine (WEB2)

| Path | What it returns |
|------|-----------------|
| `/map`, `/app.js` | The map page and its script (pan/zoom, click-a-block history, time slider, overlay). |
| `/info`, `/head` | Tile size / zoom / height band / sequence head. |
| `/tile?zoom&tx&tz` | A raw RGBA tile of the live world. |
| `/tile_at?zoom&tx&tz&at=<seq>` | The same tile replayed at a past sequence number — the "time slider". |
| `/surface?x&z[&at]` | The column's top block. |
| `/history?x&y&z` | Every change to one block, oldest first. |
| `/changes?x&z&r[&since&until]` | Block changes in a radius over a time window. |
| `/player?id=<uuid>[&since&until&x&z&r&limit]` | One player's place/break/take/put history. |
| `/overlay` | Live player/entity marks. |

Everything is read-only. Unknown routes are 404; non-`GET` requests other than `POST /login` are 405.
A browser asking for a page without a token gets the login form; a request the page's own JS makes
(`/api/…`, `/tile…`) gets a plain `401`.

## The region metrics — what is sampled and what is not

The region table is deliberately cheap: it samples **once a second** on the tick thread and reads
only snapshots, never per-block on the hot path. Each field is:

| Column | Meaning |
|--------|---------|
| `entities` | non-player entities standing in the region at the sample |
| `block_entities` | loaded containers (chests, furnaces, hoppers, brewing stands) in the region |
| `block_updates` | scheduled block/fluid ticks still pending in the region |
| `redstone_updates` | **always zero** until the redstone engine is wired to the server (it is a standalone crate today) |
| `chunk_sends` | columns sent to clients (join burst and streamer) whose centre is in the region, since the last sample |
| `cost` | `entities + 2·block_entities + block_updates + redstone + chunk_sends/4` — ranking only |

Regions not touched in a window decay (cost halves, dropped at `samples == 0`), so the heat map is a
short moving average rather than a lifetime total. The numbers are a **sample**, not a profiler: a
spike between two samples is not attributed. This is the "cheap, sampled" figure the brief asks for;
the in-engine profiler is a later task.

## Security notes

* Binds to loopback by default; the operator must opt into another interface.
* Read-only: the HTTP layer has no route that could write the world, and the `ops` service trait has
  no method that could.
* The token comparison is length-checked and byte-accumulated, not an early-exit `==`.
* Responses carry `Cache-Control: no-store`; the cookie is `HttpOnly; SameSite=Strict`. A token
  presented as `?token=` on a page request is upgraded to that cookie so the page's own JS fetches
  are authenticated too.
* No secrets in git: `token` is operator config (a sample file has none), and the one-time token is
  generated from `/dev/urandom` at runtime.

### DoS limits

The socket layer bounds what one client can do, so the console cannot be trivially starved even if
the operator binds it beyond loopback:

| limit | default | config | what it stops |
|-------|---------|--------|---------------|
| global connections | 32 | `max_conns` | a thread/memory flood from many sockets |
| per-IP connections | 8 | `max_per_ip` | one address opening unbounded sockets |
| worker pool | bounded (≤16 workers, bounded queue) | — | unbounded thread growth; over-capacity connections get `503` |
| request line | 8 KiB | fixed | an endless request line |
| header count / size | 64 / 16 KiB | fixed | an endless header stream |
| body | 64 KiB | fixed | an oversized login form |
| request deadline | 10 s | `request_deadline_ms` | slowloris (one byte per read); the victim gets `408` |
| expensive-endpoint rate | 50/s, burst 200 | `rate_per_sec`, `rate_burst` | hammering `/tile`, `/tile_at`, `/history`, `/changes`, `/player`; over-rate gets `429` |
| `/changes` radius / results / scan | 256 blocks / 4096 rows / 100 k events | fixed | one query over an unbounded area or journal |

`/tile_at` tiles are cached by `(tile, at)`, so scrubbing the time slider does not recompute the same
replayed tile; live tile rendering snapshots a whole column under one lock instead of locking per
block.

## Limits

* One thread per connection, no keep-alive (the server closes after each response). Fine for an
  operator console on loopback; not built for public traffic.
* The profile store keeps the last 16 addresses per player.
* The ender chest view is in the shape (`InventoryView::ender`) but the engine has no ender chest
  yet, so it is empty.
* The engine does not persist a player's first-join time or playtime anywhere else; the web admin's
  store (`WB` + uuid in the world KV) is the only record, written on join and leave.

## Tests

* `crates/aether-server/src/web.rs` — region indexing, uuid handling, inventory view shaping, the
  tick window.
* `plugins/webadmin` — auth (config and one-time tokens, cookie parsing, constant compare), HTTP
  parsing (method/path/query/headers, percent decoding, response encoding), routing, and page
  rendering against a mock `OpsService`.
