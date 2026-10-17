# worldedit — selection, fill and history (CMD-36a)

A **native** (tier N) Aether plugin: a Rust crate linked into the server
in-process. It owns the player selection and the `//` commands; the batched
writes, atomic journal revision, undo journal and budgeted publication are the
**core** bulk-edit API (`aether_server::bulk`, CMD-30a/30b). It never keeps a
second write path.

- Task: [`docs/tasks/CMD-36a-worldedit-selection.md`](../../docs/tasks/CMD-36a-worldedit-selection.md)
- Umbrella: [`docs/tasks/CMD-36-worldedit-plugin.md`](../../docs/tasks/CMD-36-worldedit-plugin.md)
- Core: [`docs/design/bulk-edit.md`](../../docs/design/bulk-edit.md)

## Commands

Commands are typed in the familiar double-slash form (`//set stone`). The host
command registry stores the name without the slash, so the plugin declares
`set`, `pos1`, … and the same node gates the command.

| command | syntax | permission node |
|---|---|---|
| wand | `//wand` | `worldedit.wand` |
| positions | `//pos1 [<pos>]`, `//pos2 [<pos>]` | `worldedit.selection` |
| selection | `//sel`, `//expand <n> [dir]`, `//contract <n> [dir]` | `worldedit.selection` |
| fill | `//set <pattern>` | `worldedit.region.set` |
| replace | `//replace [<filter>] <pattern>` | `worldedit.region.replace` |
| walls | `//walls <pattern>` | `worldedit.region.walls` |
| outline | `//outline <pattern>` | `worldedit.region.outline` |
| history | `//undo [<n>]`, `//redo [<n>]` | `worldedit.history.undo` / `worldedit.history.redo` |

Every node is declared with op level 0, so an operator can grant it to a
non-op with the normal permission model. The command registry checks the node
before the plugin sees the command, exactly like a core command.

`//pos1` / `//pos2` without an argument use the player's current block cell.
With arguments they use the core coordinate parser (CMD-06a), so absolute,
`~`-relative and `^`-local components resolve the same way as in vanilla
commands. A position is stored with the player's current world.

`//expand` and `//contract` accept `north`/`south`/`east`/`west`/`up`/`down`
(also `n s e w u d`). Without a direction they use the player's horizontal
facing. Contraction that would collapse an axis is refused.

## Patterns

`//set`, `//walls` and `//outline` take a pattern; `//replace` takes an optional
filter and then a pattern.

- `stone` — a block name; the `minecraft:` namespace may be omitted.
- `50%stone,50:dirt` — weighted choices. Both `%` and `:` are accepted as the
  weight separator, and the weights need not add up to 100.
- `#existing` — the block already at each position.
- `#hand` — the block held in the main hand.
- `#pos1` — the block at selection position 1.
- `#clipboard` — the clipboard pattern. Copy/cut/paste is CMD-36b; until then
  the clipboard is empty and this pattern reports an error.

`//replace`'s filter is a comma-separated list of block names (a weight prefix
is accepted and ignored) or `#existing` for “not air”. Without a filter the
whole selection is replaced, including air, as WorldEdit does. Filters match
the block type, so `stone` matches every stone state.

An unknown block, an empty entry, a zero weight or an unknown `#pattern` is an
error and no edit starts.

## Selection and the wand

- `//wand` gives the configured wand item (default `minecraft:wooden_axe`).
- Holding the wand, **left-click** a block sets position 1 and **right-click**
  sets position 2. Both clicks are cancelled, so the wand never breaks,
  places or opens anything. The hooks are the native-only `before-block-dig`
  and `before-block-interact`, so a 1.8.9 client's tool right-click also
  selects.
- `//sel` prints the corners and the inclusive volume.

The selection is per session, is reset when the player leaves, and is not
persisted across a restart. This is the WorldEdit session model (a stale
selection from a reused entity id is worse than no selection).

## Limits, region protection and attribution

- The maximum edit volume is the core bulk-edit limit
  (`aether_server::bulk::DEFAULT_MAX_BLOCKS`, 1,000,000 distinct positions).
  A selection over it is refused **before** an edit handle is created.
- The plugin stages a whole operation first; a failed item aborts the edit and
  leaves the world untouched. A successful commit is one atomic journal
  revision (`EventBody::BlockGroup`) attributed to the player's UUID.
- Publication — shape settling, chunk-column re-sends and the after-* hooks —
  is the core `BulkPublisher`, driven by the `[bulk]` per-tick budget
  (`cells_per_tick`, `columns_per_tick`, `hooks_per_tick`). The plugin does not
  invent its own tick budget.
- Before an edit the plugin asks the core region check (CMD-29b) about every
  cell in the selection with `Action::Place`. If any protected region denies
  it, the whole edit is refused. The existing bypass nodes
  (`aether.region.bypass` and `worldguard.region.bypass.<world>`) are honoured
  by the core check; the plugin has no separate bypass.

Undo/redo keeps a bounded per-player stack (default 20, configurable up to
1000). Undo is a new journal revision attributed to the same author, so it is
auditable and can itself be undone; `//redo` is the undo of that revision. A
new edit clears the redo stack.

## Configuration

Values are read through the plugin host's `config` import (never from a file of
its own):

```toml
[plugins.worldedit]
enabled = true
config = { wand-item = "minecraft:wooden_axe", history-size = "20" }
```

The plugin asks for `log`, `config`, `supports`, `storage.kv`, `world.read`,
`world.write` and `players`. As with every native plugin, grants are
deny-by-default: list the capabilities under `[plugins.worldedit]` as well.

The edit path is a host service, `worldedit.host@0.1`, declared in
`crates/aether-plugin/src/worldedit.rs` and implemented in
`crates/aether-server/src/worldedit.rs`. The service owns the `BulkEdit`
handles, the `BulkHistory`, the region check and the coordinate parsing; the
plugin owns pattern parsing, selection state, command grammar and history
stacks. A native plugin cannot depend on `aether-server` without a cycle, so
the service is the API seam.

## Wiring

The server compiles the plugin behind its `plugins` feature and installs it in
`install_native` (`crates/aether-server/src/plugins.rs`), after registering the
`worldedit.host@0.1` service. It still requires `[plugins] dir` as the
subsystem master switch and `[plugins.worldedit] enabled = false` skips it.

## Tests

```sh
cargo-locked test -p aether-worldedit-plugin -p aether-server --lib worldedit
```

The plugin suite covers: a legal `//set` committing once, an over-limit
selection refused before any edit starts, weighted and `#hand` patterns, an
invalid pattern, `//replace` with and without a filter, walls and outline, a
multi-step undo/redo with bounded history, explicit and facing-relative
expand/contract, a selection across chunk borders, a protected region refusing
the edit, selection reset on leave, the wand and its two clicks, absolute and
relative positions. The server suite covers the real service over the core
`BulkEdit`/journal (one revision, undo restores exactly), the core limit,
region denial plus an operator bypass, the core coordinate parser, block
resolution and hand lookup.

## Not in this slice

Clipboard (`//copy`, `//cut`, `//paste`), transforms (`//rotate`, `//flip`,
`//stack`, `//move`), schematics and brushes are CMD-36b. `#clipboard` is
recognised now so a pattern fails clearly rather than being read as a block
name.
