# regions — WorldGuard-style regions (CMD-34a)

A **native** (tier N) Aether plugin: a Rust crate linked into the server
in-process. It owns the region model, the `/rg` commands and persistence. The
hot-path cuboid lookup and priority/flag check are the **core** implementation
(`aether-region`, built for CMD-29a/29b) — the plugin edits the one shared
`RegionStore` the protection hooks read and never keeps a second spatial index.

- Task: [`docs/tasks/CMD-34a-regions-model.md`](../../docs/tasks/CMD-34a-regions-model.md)
- Umbrella: [`docs/tasks/CMD-34-regions-plugin.md`](../../docs/tasks/CMD-34-regions-plugin.md)
- Next slice: CMD-34b (`/rg flag`, members/owners, per-world bypass)

## Commands

`/rg` and `/region` are the same command.

| command | syntax | permission node |
|---|---|---|
| define | `/rg define <id>` (from the wand selection) | `worldguard.region.define` |
| redefine | `/rg redefine <id>` | `worldguard.region.redefine` |
| remove | `/rg remove <id>` | `worldguard.region.remove` |
| priority | `/rg priority <id> <n>` | `worldguard.region.priority` |
| info | `/rg info [<id>]` | `worldguard.region.info` |
| list | `/rg list [<player>]` | `worldguard.region.list` |

- `define` validates the id (`[A-Za-z0-9_-]`, 1–32 chars), rejects a duplicate,
  and makes the caller the region's owner.
- `redefine` moves the bounds **atomically**: one new core index snapshot and
  one KV write. A reader holding an older snapshot keeps the old, whole box;
  the next snapshot has the new, whole box. No half-applied box is visible.
- `remove` publishes the removal immediately; callers already holding a
  snapshot keep answering from it until the next safe point, which is the
  core's documented snapshot contract.
- `priority` changes the order the core check consults overlapping regions.
  Higher wins; equal priorities are broken by the stable numeric id (ascending),
  so the answer is deterministic across restarts.
- `info` with an id shows its world, bounds, priority and owners; without an id
  it shows the region under the player (highest priority first).
- `list` groups every region by world; an optional player name filters to
  regions that player owns.

A non-op may edit only a region they own. A region with **no owner** is
therefore operator-only. Operators bypass this plugin-side gate.

## Selection: the internal wand

WorldEdit (CMD-36) is **not in main yet**, so this plugin ships the smallest
useful selection tool itself:

1. hold the configured wand item (default `minecraft:golden_axe`);
2. **left-click** a block — point 1;
3. **right-click** a block — point 2;
4. `/rg define <id>` or `/rg redefine <id>`.

Both clicks are cancelled while holding the wand, so it never breaks, places,
opens or toggles anything. Bounds are normalized on every axis, so the click
order does not matter, and a selection may span chunk borders (the core index
enters the cuboid into every chunk column it touches).

The wand item is `[plugins.regions] config = { wand-item = "minecraft:wooden_axe" }`
(`minecraft:` may be omitted). The selection is per session and is not
persisted; only the regions are.

Right-click at a block uses `before-block-interact`, a **native-only** event
(the frozen WASM `@0.1.0` world has no export for it). A WASM plugin can
subscribe to the name but is skipped; the native plugin receives the clicked
cell, which the wand needs.

## Persistence

The whole region set is one JSON value in the plugin KV under the key
`regions`. Every mutation updates the in-memory model, publishes one core
snapshot and writes the key once. On `init` the plugin loads the key, rebuilds
the core store from it, and keeps the stable numeric ids (the priority
tie-break) and the next id counter. State therefore survives a server restart.

No world file is edited directly: the server's `storage.kv` backend owns the
bytes, exactly like any other plugin state.

## Wiring

The server compiles the plugin behind its `plugins` feature and installs it in
`install_native` (`crates/aether-server/src/plugins.rs`). It is enabled like any
other plugin and still requires `[plugins] dir` to be set (the subsystem's
master switch); `[plugins.regions] enabled = false` skips it. The plugin needs
the `log`, `config`, `supports`, `storage.kv`, `world.read` and `players`
capabilities; grants are deny-by-default like every plugin.

## Tests

```sh
cargo-locked test -p aether-region -p aether-regions-plugin
```

The plugin suite covers a chunk-border-spanning define, an invalid/duplicate
id, an atomic redefine (old snapshot vs new), per-world `list`, remove at the
next safe point, an unowned region refusing a non-op edit, restart persistence,
deterministic priorities and equal-priority ties, the two-click wand, and that
the plugin edits the same core `RegionStore`/`RegionIndex` the check uses.

## Not in this slice

`/rg flag`, `/rg addmember`/`addowner` and per-world bypass are CMD-34b. Polygon
regions are later; this slice is cuboids only.
