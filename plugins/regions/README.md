# regions — WorldGuard-style regions (CMD-34a/34b)

A **native** (tier N) Aether plugin: a Rust crate linked into the server
in-process. It owns the region model, the `/rg` commands and persistence. The
hot-path cuboid lookup and priority/flag check are the **core** implementation
(`aether-region`, built for CMD-29a/29b) — the plugin edits the one shared
`RegionStore` the protection hooks read and never keeps a second spatial index.

- Model/commands: [`docs/tasks/CMD-34a-regions-model.md`](../../docs/tasks/CMD-34a-regions-model.md)
- Flags/members/bypass: [`docs/tasks/CMD-34b-regions-flags-members.md`](../../docs/tasks/CMD-34b-regions-flags-members.md)
- Umbrella: [`docs/tasks/CMD-34-regions-plugin.md`](../../docs/tasks/CMD-34-regions-plugin.md)

## Commands

`/rg` and `/region` are the same command.

| command | syntax | permission node |
|---|---|---|
| define | `/rg define <id>` (from the wand selection) | `worldguard.region.define` |
| redefine | `/rg redefine <id>` | `worldguard.region.redefine` |
| remove | `/rg remove <id>` | `worldguard.region.remove` |
| priority | `/rg priority <id> <n>` | `worldguard.region.priority` |
| flags | `/rg flag <id> <flag> [value]` | `worldguard.region.flag` |
| members | `/rg addmember <id> <player>` / `/rg removemember <id> <player>` | `worldguard.region.member` |
| owners | `/rg addowner <id> <player>` / `/rg removeowner <id> <player>` | `worldguard.region.owner` |
| info | `/rg info [<id>]` | `worldguard.region.info` |
| list | `/rg list [<player>]` | `worldguard.region.list` |
| cancel | `/rg cancel` | none |
| bypass | (permission) | `worldguard.region.bypass.<world>` |

`/rg` itself is a public top-level command; the `worldguard.region.*` nodes
gate the individual subcommands inside the plugin. A deny on `info` therefore
cannot block `define` before the arguments are read.

- `define` validates the id (`[A-Za-z0-9_-]`, 1–32 chars), rejects a duplicate,
  records the caller's current world, and makes the caller the region's owner.
- `redefine` moves the bounds **atomically**: one new core index snapshot and
  one KV write. A reader holding an older snapshot keeps the old, whole box;
  the next snapshot has the new, whole box. No half-applied box is visible. It
  keeps the region's stored world; a selection carries no dimension, so taking
  the caller's current world would silently move a region created elsewhere.
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
- `flag` sets a **typed** flag from the core registry (CMD-29b).
  Boolean flags accept `allow`/`deny` (also `true`/`false`, `yes`/`no`,
  `on`/`off`); `greeting`, `farewell` and the other text flags take the rest of
  the line; integer flags such as `heal-amount` take a whole number.
  `none`/`unset` clears a flag, and no value shows its current setting. An
  unknown flag or a value of the wrong type is an error and changes nothing.

  The registry is a **fixed built-in set for 1.0** (there is no plugin flag
  registration yet). Of the boolean names, only the ones the core check
  consults are effective: **`build`** (break/place), **`use`**
  (interaction/item use), **`pvp`**, **`explosion`** and **`entry`**.
  `entry` is **stored-only** for now: the flag is validated, persisted and
  honoured by an explicit `Action::Entry` check, but the server has no
  teleport/entry hook yet. `interact`, `item-use`, `construct`, `chest-access`,
  `other-explosion`, `mob-damage` and the other compatibility boolean names are
  accepted and persisted but **inert** — `/rg flag <id> interact deny` reports
  success and changes nothing. Text and integer flags are likewise stored and
  shown by `info` only. An SDK/WASM reader sees only the boolean flags through
  `RegionInfo`; `greeting`/`heal-amount` are not exposed there yet.
- `addmember`/`removemember` and `addowner`/`removeowner` edit the persisted
  membership. Members and owners may build/use a region while the corresponding
  flag is unset; an explicit `deny` still wins. A stranger is denied unless a
  flag explicitly allows the action.

A non-op may edit only a region they own. A region with **no owner** is
therefore operator-only. Operators bypass this plugin-side gate.

## Bypass

Region protection itself is the core check. The server hook resolves the
player's current session world and checks the region in that same world. A
player with `worldguard.region.bypass.<world>` (or the core
`aether.region.bypass`) skips it in that world; operators hold the undeclared
node by default. The node is per-world: a grant for `overworld` does not bypass
`the_nether`, and the hook's per-world resolution is covered by
`crates/aether-server/src/region.rs` tests. Without the node a non-operator
remains subject to the flags, member or not.

## Selection: the internal wand

WorldEdit (CMD-36) is **not in main yet**, so this plugin ships the smallest
useful selection tool itself:

1. hold the configured wand item (default `minecraft:golden_axe`);
2. **left-click** a block — point 1;
3. **right-click** a block — point 2;
4. `/rg define <id>` or `/rg redefine <id>`.

Both clicks are cancelled while holding the wand, so it never breaks, places,
opens or toggles anything. Point 1 is taken at the **start of the left click**
(`before-block-dig`), before the region check and before a survival dig begins,
so a protected or unbreakable block can still be selected; point 2 is taken on
the right click (`before-block-interact`), including on 1.8.9 where a tool
right-click used to be dropped by the codec. Bounds are normalized on every
axis, so the click order does not matter, and a selection may span chunk
borders (the core index enters the cuboid into every chunk column it touches).

The selection is cleared after a successful `define`/`redefine`/`remove`, by
`/rg cancel`, and when the player leaves (`after-player-leave`), so a reused
session id cannot inherit someone else's points.

The wand item is `[plugins.regions] config = { wand-item = "minecraft:wooden_axe" }`
(`minecraft:` may be omitted). The selection is per session and is not
persisted; only the regions are.

Right-click at a block uses `before-block-interact`, a **native-only** event
(the frozen WASM `@0.1.0` world has no export for it). A WASM plugin can
subscribe to the name but is skipped; the native plugin receives the clicked
cell, which the wand needs.

## Persistence

The whole region set is one JSON value in the plugin KV under the key
`regions`: bounds, priority, typed flags, members and owners. Every mutation
writes the new value **first** and only then adopts it in memory and publishes
one core snapshot, so a failed write leaves the previous, still-persisted state
in place and the command reports the error. On `init` the plugin loads the key,
rebuilds the core store from it, and keeps the stable numeric ids (the priority
tie-break) and the next id counter. Flags/members/owners therefore survive a
server restart. Boolean flags written by CMD-34a still load (the typed value is
`untagged`), so the upgrade needs no migration.

Loading is fail-closed: an unreadable value or a failed KV read refuses the
plugin load rather than starting with an empty set and overwriting recoverable
bytes on the next edit. The server logs that at **error** level and states that
region protection is not active. An operator who accepts an empty start after
such a failure may set `[plugins.regions] config = { allow-empty-on-error = "true" }`;
even then the unreadable bytes are never rewritten until a later successful
edit.

No world file is edited directly: the server's `storage.kv` backend owns the
bytes, exactly like any other plugin state.

## Wiring

The server compiles the plugin behind its `plugins` feature and installs it in
`install_native` (`crates/aether-server/src/plugins.rs`). It is enabled like any
other plugin and still requires `[plugins] dir` to be set (the subsystem's
master switch); `[plugins.regions] enabled = false` skips it. The plugin needs
the `log`, `config`, `supports`, `storage.kv` and `players` capabilities (it
reads no blocks, so `world.read` is not requested); grants are deny-by-default
like every plugin.

## Tests

```sh
cargo-locked test -p aether-region -p aether-regions-plugin
```

The plugin suite covers a chunk-border-spanning define, an invalid/duplicate
id, an atomic redefine (old snapshot vs new), per-world `list`, remove at the
next safe point, an unowned region refusing a non-op edit, restart persistence
checked against the **new** store, deterministic priorities and equal-priority
ties, the two-click wand (including its start-of-dig hook), selection reset and
cancel, world preservation on redefine, failed-write rollback, fail-closed load,
and that the plugin edits the same core `RegionStore`/`RegionIndex` the check
uses. CMD-34b adds: `build` deny blocking break/place and `use`/`entry`/`pvp`
following their actions, the next-safe-point snapshot behavior of a flag edit,
members/owners gaining and losing rights, member/owner add/remove, typed
flag/member persistence across restart, invalid flag/value errors, and the
per-world bypass node. The 1.8.9 codec has its own test that a tool right-click
becomes the selection event.

## Not in this slice

Polygon regions, region groups and the runtime greeting/farewell delivery are
later; this slice is cuboids, typed flags, membership and per-world bypass.
