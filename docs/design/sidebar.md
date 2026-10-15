# Sidebar scoreboard — native plugin `plugins/sidebar`

Status: ✅ implemented (2026-10-15), queue SB1. Tier **N** (native plugin) on the plugin API, with a
small core mechanism underneath. This document is the as-built record; the task is in
[`QUEUE-SB-sidebar.md`](../tasks/QUEUE-SB-sidebar.md).

> **Read this in other languages:** [Русский 🇷🇺](sidebar.ru.md)

## Why

Every server wants a per-player panel on the right: name, rank, coins, ping, online count, TPS,
kills. Shops, economy and ranks all want to put a line there. This is one shared, flicker-free
implementation, so plugins do not each reinvent the scoreboard packets (and get the per-version
details wrong).

## Tier: core mechanism, plugin policy

The split follows [`plugins.md`](plugins.md)'s rule — the core keeps mechanisms, a plugin keeps
policy.

| | where | what |
|---|---|---|
| **core** (`crates/aether-server`) | `protocol/scoreboard.rs` + `PlayerHandle` | the neutral sidebar model, the explicit scoreboard operations, the per-version `Wire` table, and the per-player packet API (`update_sidebar`, `set_tab_header_footer`) with the flicker-free diff |
| **plugin** (`plugins/sidebar`) | the layouts, conditions and placeholder values | which lines a player sees, resolved from `config.toml` and `%placeholders%`, at a rate limit |

A native plugin compiles into the server, so it can be disabled like any other
(`[plugins.sidebar] enabled = false`), and the core runs correctly without it. The plugin never
builds packets; it hands the core a `SidebarBoard` and the core renders it in each client's own
dialect.

## Core: the per-player packet API

`SidebarBoard { title, lines }` (up to fifteen lines, plain text with `§` codes). A host pushes it
through a `HostBackend` method:

```rust
backend.set_sidebar(plugin, player, &board);        // show / update
backend.clear_sidebar(plugin, player);              // hide
backend.set_tab_header_footer(plugin, player, &header, &footer);
```

`ServerBackend` maps these onto `PlayerHandle::update_sidebar` / `set_tab_header_footer`, which hold
the player's last board and header/footer and diff against them. **Only changed lines produce a
packet.** An unchanged tick writes nothing to the socket, which is what keeps a periodically
re-evaluated sidebar from flickering.

The diff yields explicit `ScoreboardOp`s (`ObjectiveAdd`, `Display`, `TeamAdd`, `TeamUpdate`,
`ScoreAdd`, `ResetScore`, …). A `Wire` (packet ids + field shapes) turns each op into bytes; the
wire is chosen from the codec's `protocol_id()` by `scoreboard::wire_for`, so **adding a release is
one row in one table**, not an edit to every codec module. `PlayerHandle::render` handles the
scoreboard and tab-list events itself, so the twenty-four codec `encode`s are untouched.

### Two rendering strategies

- **Up to 1.20.2 (`SidebarStyle::Teams`)**: the visible text rides in a team's prefix/suffix; the
  score entry is an invisible colour code. A line change rewrites that team's prefix — one packet
  — so the objective, the display slot and the other lines are never torn down.
- **1.20.3+ (`SidebarStyle::DisplayNames`)**: the text rides in the score's own display name and
  `number_format = blank` hides the red number. No teams at all. A line change is one score packet.

The strategy is derived from whether the version has a number format (`Wire::number_format`).

### Wire coverage

| protocols | versions | text | teams | display slot | reset |
|---|---|---|---|---|---|
| 47 | 1.8.9 | raw `§` string | 1.8 layout, 16-char halves | byte | score action `1` |
| 755–762 | 1.17–1.19.4 | JSON component | classic, 64-char halves | byte | score action `1` |
| 763–764 | 1.20/1.20.1, 1.20.2 | JSON component | classic | byte / VarInt | score action `1` |
| 765–772 | 1.20.3–1.21.8 | NBT component | NBT strings, classic order | VarInt | `reset_score` |
| 773–774 | 1.21.9–1.21.11 | NBT component | NBT strings + flags byte | VarInt | `reset_score` |
| 775–777 | 26.1–26.3 | NBT component | as 1.21.11 | VarInt | `reset_score` |

Truncation: objective names stay short; 1.8 splits a long line across a 16-char prefix and a
16-char suffix (a split never leaves a dangling `§`); from 1.13 the halves are 64 characters. The
title is `aether.side`, clear of the vanilla `/scoreboard` names.

Tab list: the `playerlist_header` packet (a different id in every generation, `message_header` in
1.19.2), JSON before 1.20.3 and NBT from 1.20.3.

## Plugin: config, layouts, placeholders

`plugins/sidebar/config.toml` is data-only:

```toml
title = "&6&lAether"
update_hz = 4          # times per second; default 4, capped at 20
tab = false            # mirror the board into the tab list header/footer
clock = "world"        # what %time% shows: "world" (default) | "server"
mode = "override"      # "override" | "yield" (see limitations)

[[layout]]
priority = 0
lines = ["&7Online: &f%online%&7/&f%max%", "&7Ping: &f%ping%ms", "&e%player%"]

[[layout]]
priority = 10
permission = "sidebar.vip"
world = "minecraft:the_end"
lines = ["&dVIP", "&7Online: &f%online%"]
```

The highest-priority layout whose conditions pass is drawn. Conditions: `permission` (needs the SDK
permission service; fails closed while CMD-01/02 are absent), `world`, and `placeholder`/`equals`.

**Built-in placeholders**: `%player%`, `%displayname%`, `%ping%`, `%online%`, `%max%`, `%world%`,
`%x%` `%y%` `%z%`, `%tps%`, `%health%`, `%level%`, `%time%` (world or server clock per the `clock`
setting), `%world_time%` and `%server_time%` (explicit). `%ping%` and `%tps%` are measured, not
invented: the core times the keep-alive round trip and smooths the actual tick rate. `&`-colour
codes are translated to `§`. An unknown token is left literal, so a typo shows up instead of
vanishing.

**Other plugins** register their own (`%economy_balance%`, `%rank_prefix%`) through the `sidebar@0.1`
service registered in the host's service registry:

```rust
// native consumer: resolve the service once, then
sidebar.set_line(player, 3, "&6%economy_balance%".into());
sidebar.register_placeholder("economy_balance".into(), Arc::new(|q| economy.balance(q.player)));
sidebar.toggle(player);   // /sidebar toggle
```

The **plugin** also registers a `/sidebar toggle|show|hide|reload` command (through the core command
registry, so it is permission-checked like any other).

## Tests

- `crates/aether-server/src/protocol/scoreboard.rs`: the diff (empty on unchanged, one packet per
  changed line, add/remove lines, title-only rename, hide), the fifteen-line cap, 1.8 splitting, and
  wire-level encoding per family — 1.8 string criterion and score action, JSON components, modern
  NBT + blank number format + `reset_score`, the 1.8 vs modern team field order, and the tab list.
- `plugins/sidebar`: config parsing, placeholder resolution + colour translation, and an end-to-end
  test that drives the real `NativePlugin` through a recording `HostBackend` (board pushed and
  resolved, service placeholder + line override, toggle, tab mirror).

Checks per [`COMMON.md`](../tasks/COMMON.md): `cargo fmt`, `cargo clippy -- -D warnings`,
`cargo-locked test`.

## Limitations

- **CMD-15 is not landed.** The core keeps only what a sidebar needs (the sidebar model + these
  packets). The full vanilla `/scoreboard` model, commands and storage are a separate task
  (CMD-15a/b); `mode = "yield"` is parsed but inert until there is a vanilla sidebar model to yield
  to.
- **Player `%level%` is 0.** The experience level is tracked on `PlayerState` but not routed into
  `PlayerExtra` yet; ping, world time and TPS are real.
- **WASM plugins cannot register placeholders yet.** The `sidebar@0.1` service is typed in
  `crates/aether-plugin` for native consumers. A WASM consumer needs a WIT interface (a new package
  version, since exports are frozen), which is deferred.
- **26.1–26.3 scoreboard packet ids are unverified** — no data-generator report for those releases
  was available when the table was written, so they reuse 1.21.11's ids. The field shapes are
  correct; a wrong id would be corrected by regenerating the table from the release's own
  `packets.json`.
