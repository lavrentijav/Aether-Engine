# worldedit — selection, clipboard, schematics and brushes (CMD-36)

A **native** (tier N) Aether plugin: a Rust crate linked into the server
in-process. It owns the player selection, clipboard, history, schematics and
brushes; the batched writes, atomic journal revision, undo journal and budgeted
publication are the **core** bulk-edit API (`aether_server::bulk`, CMD-30a/30b).
It never keeps a second write path.

- Selection/fill task: [`docs/tasks/CMD-36a-worldedit-selection.md`](../../docs/tasks/CMD-36a-worldedit-selection.md)
- Clipboard task: [`docs/tasks/CMD-36b-worldedit-clipboard.md`](../../docs/tasks/CMD-36b-worldedit-clipboard.md)
- Umbrella: [`docs/tasks/CMD-36-worldedit-plugin.md`](../../docs/tasks/CMD-36-worldedit-plugin.md)
- Core: [`docs/design/bulk-edit.md`](../../docs/design/bulk-edit.md)

## Commands

Commands are typed in the familiar double-slash form (`//set stone`). The host
command registry stores the name without the slash, so the plugin declares
`set`, `pos1`, … and the same node gates the command.

| command | syntax | permission node | op |
|---|---|---|---|
| wand | `//wand` | `worldedit.wand` | 0 |
| positions | `//pos1 [<pos>]`, `//pos2 [<pos>]` | `worldedit.selection` | 0 |
| selection | `//sel`, `//expand <n> [dir]`, `//contract <n> [dir]` | `worldedit.selection` | 0 |
| fill | `//set <pattern>` | `worldedit.region.set` | 2 |
| replace | `//replace [<filter>] <pattern>` | `worldedit.region.replace` | 2 |
| walls | `//walls <pattern>` | `worldedit.region.walls` | 2 |
| outline | `//outline <pattern>` | `worldedit.region.outline` | 2 |
| copy | `//copy` | `worldedit.clipboard.copy` | 0 |
| cut | `//cut` | `worldedit.clipboard.cut` | 2 |
| paste | `//paste [-a] [-o] [-s]` | `worldedit.clipboard.paste` | 2 |
| rotate | `//rotate <y> [x] [z]` | `worldedit.clipboard.rotate` | 0 |
| flip | `//flip [dir]` | `worldedit.clipboard.flip` | 0 |
| stack | `//stack <n> [dir]` | `worldedit.clipboard.stack` | 2 |
| move | `//move <n> [dir]`, `//move <dir> <n>` | `worldedit.region.move` | 2 |
| schematics | `//schem save\|load\|list\|paste <name>` | `worldedit.schematic` | 2 |
| brush | `//brush sphere\|cyl\|smooth\|clear`, `/brush` | `worldedit.brush` | 2 |
| mask | `//mask [<filter>]`, `/mask clear` | `worldedit.brush.mask` | 0 |
| history | `//undo [<n>]`, `//redo [<n>]` | `worldedit.history.undo` / `worldedit.history.redo` | 2 |

Selection, the wand, reading the clipboard into memory and changing the
clipboard's orientation are **op 0**. Commands that write the world — directly
or through a brush — and the history that can replay those writes are **op 2**,
the same level as the core `/fill`, `/setblock` and `/clone`. `//schem` is op 2
as a whole because `//schem paste` writes the world and `save` writes a file.
An operator who wants a player to build with WorldEdit grants the relevant
nodes (CMD-01/02); the command registry checks the node and op level before the
plugin sees the command, exactly like a core command.

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
- `#clipboard` — the copied block at the same relative position in the
  selection. A selection larger than the clipboard, or an empty clipboard, is
  an error and no edit starts.

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

## Clipboard

`//copy` captures the selection into an in-memory per-player clipboard: its
size, the block state at every cell (air included) and its offset relative to
the player's block position at copy time. The clipboard is reset when the
player leaves. It is deliberately not persisted; schematics are.

`//paste` places the clipboard at the player's current position plus that
offset, so staying still reproduces the copy in place and walking somewhere
else shifts it by the same delta.

- Default: every clipboard cell is written only where the target is air, so a
  paste does not demolish existing builds.
- `-o`: overwrite — write every cell regardless of what is at the target,
  air included.
- `-a`: skip air — do not write air from the clipboard (useful with `-o` to
  stamp a shape without a bounding-box hole).
- `-s`: select the pasted region after the edit (the clipboard footprint, not
  just the non-air cells).
- An unknown option, an empty clipboard, or a clipboard over the core limit is
  an error **before** an edit handle is created.

`//cut` copies the selection and then replaces it with air in one atomic
revision. The clipboard still holds the cut blocks, including air.

`//rotate <y> [x] [z]` rotates the clipboard around the box centre in 90°
steps. Angles are applied in Y, X, Z order; every angle must be a multiple of
90 (negative angles and full turns are accepted). `//flip [dir]` mirrors along
the axis named by the direction: north/south flips Z, east/west flips X,
up/down flips Y; without a direction it uses the player's facing. Flipping
twice with the same direction returns the original.

`//stack <n> [dir]` copies the selection `n` times in a direction, stepping by
the selection's size on that axis. It is one bulk commit. The default direction
is the player's facing; the selection is expanded to cover the source and all
copies. A stack whose total volume exceeds the limit is refused before the
edit starts.

`//move <n> [dir]` (also `//move <dir> <n>`) moves the selection by `n` blocks
in one atomic revision: the source becomes air and the destination receives
the copied snapshot, including overlap. The selection follows the destination.
Both the source and destination cells are region-checked.

## Schematics

`//schem save <name>` writes the clipboard as a gzip-NBT `.schem` file
(Sponge schematic version 2: `Version`, `DataVersion`, `Width`, `Height`,
`Length`, `Offset`, `Palette`, `PaletteMax`, `BlockData` and an empty
`BlockEntities` list). `//schem load <name>` reads one back; `//schem list`
prints the names; `//schem paste <name>` loads and pastes in one command,
accepting the same `-a`/`-o`/`-s` flags.

Names are a single file stem: ASCII letters, digits, `_` and `-`, at most 64
characters. The host validates the name again and writes under
`<world_dir>/schematics/<name>.schem`, so a name can never escape that
directory. The files survive a restart because the plugin sends bytes over the
`worldedit.host` service and the host owns the filesystem.

## Brushes

`//brush` (and `/brush`) binds a brush to the item held when the command runs:

- `//brush sphere <pattern> [radius]` — default radius 2.
- `//brush cyl <pattern> <radius> [height]` — default height 1; the cylinder is
  centred on the clicked cell.
- `//brush smooth [radius]` — default radius 3; removes blocks inside the
  sphere, optionally restricted by the mask.
- `//brush clear` removes the bound brush.
- `//brush` with no arguments prints the current brush.

`//mask <filter>` / `/mask <filter>` sets a brush mask with the same filter
syntax as `//replace` (`#existing` = not air, or a block list). `/mask clear`
removes it.

A brush applies with **right-click** on a block while holding its bound item;
left-click is also accepted. The click is cancelled. Every application is a
normal core bulk edit: target cells are region-checked, the volume limit is
checked before the edit, the whole application is one journal revision, and
`//undo` covers it. The bound item is the configured `brush-item` when the
player was holding nothing at bind time (default `minecraft:blaze_rod`).

## Limits, region protection and attribution

- The maximum edit volume is the core bulk-edit limit
  (`aether_server::bulk::DEFAULT_MAX_BLOCKS`, 1,000,000 distinct positions).
  A selection, clipboard, stack, move or brush over it is refused **before** an
  edit handle is created.
- The plugin stages a whole operation first; a failed item aborts the edit and
  leaves the world untouched. A successful commit is one atomic journal
  revision (`EventBody::BlockGroup`) attributed to the player's UUID.
- Publication — shape settling, chunk-column re-sends and the after-* hooks —
  is the core `BulkPublisher`, driven by the `[bulk]` per-tick budget
  (`cells_per_tick`, `columns_per_tick`, `hooks_per_tick`). The plugin does not
  invent its own tick budget.
- Before an edit the plugin asks the core region check (CMD-29b) about the
  **cells it will touch**: the selection for fill/cut, the exact writes for
  paste/stack/move and the brush sphere/cylinder. A corner of a protected
  region does not refuse an unrelated paste that only shares its bounding box.
  If any protected region denies a cell, the whole edit is refused. The
  existing bypass nodes (`aether.region.bypass` and
  `worldguard.region.bypass.<world>`) are honoured by the core check; the
  plugin has no separate bypass.
- **`//undo` and `//redo` run the same check** over the cells the revision
  actually changed, before the inverse is written. A player who filled
  wilderness cannot undo over a region somebody else defined there afterwards;
  a bypassing operator still can. The undo id is also scoped to the plugin
  that issued it, so one plugin cannot undo another's revision.

Undo/redo keeps a bounded per-player stack (default 20, configurable up to
1000). Undo is a new journal revision attributed to the same author, so it is
auditable and can itself be undone; `//redo` is the undo of that revision. A
new edit clears the redo stack. Clipboard, transforms, cut, paste, stack, move
and brush applications all go through the same stack.

## Configuration

Values are read through the plugin host's `config` import (never from a file of
its own):

```toml
[plugins.worldedit]
enabled = true
config = { wand-item = "minecraft:wooden_axe", brush-item = "minecraft:blaze_rod", history-size = "20", max-volume = "250000" }
```

The plugin asks for `log`, `config`, `world.read`, `world.write` and `players` —
only what it uses (clipboard, selection and history are in memory; schematics
go through the host service, so no `storage.kv`; nothing probes features, so no
`supports`). As with every native plugin, grants are deny-by-default: list the
capabilities under `[plugins.worldedit]` as well.

`max-volume` lowers the maximum edit volume (default and hard ceiling is the
core `aether_server::bulk::DEFAULT_MAX_BLOCKS`, 1,000,000 distinct blocks). The
same value is used by the plugin's pre-check and by `BulkEdit::with_limit`, so
the refusal message and the actual transaction agree. Schematic files live
under `<world_dir>/schematics`; there is no separate setting.

The edit path is a host service, `worldedit.host@0.1`, declared in
`crates/aether-plugin/src/worldedit.rs` and implemented in
`crates/aether-server/src/worldedit.rs`. The service owns the `BulkEdit`
handles, the `BulkHistory`, the region check, coordinate parsing and schematic
files; the plugin owns pattern parsing, selection/clipboard/brush state,
command grammar, transforms, `.schem` encoding and history stacks. A native
plugin cannot depend on `aether-server` without a cycle, so the service is the
API seam.

## Wiring

The server compiles the plugin behind its `plugins` feature and installs it in
`install_native` (`crates/aether-server/src/plugins.rs`), after registering the
`worldedit.host@0.1` service with `<world_dir>/schematics` as its schematic
directory. It still requires `[plugins] dir` as the subsystem master switch and
`[plugins.worldedit] enabled = false` skips it.

## Tests

```sh
cargo-locked test -p aether-worldedit-plugin
cargo-locked test -p aether-server --lib worldedit
```

The plugin suite covers: a legal `//set` committing once, an over-limit
selection refused before any edit starts, weighted and `#hand` patterns, an
invalid pattern, `//replace` with and without a filter, walls and outline, a
multi-step undo/redo with bounded history, explicit and facing-relative
expand/contract on every axis/direction, a selection across chunk borders, a
protected region refusing the edit, selection reset on leave, a stale
selection from another world, the wand and its two clicks, absolute and
relative positions, `#existing`/`#pos1`/`#clipboard` patterns, and the
op-level/capability manifest. For CMD-36b it adds: copy/paste in place and
after the player moves, the `-a`/`-o`/`-s` paste semantics, rotate
90/180/270 plus an invalid angle, flip twice and every axis, stack direction
and its limit, both move argument orders and source clearing, cut filling the
clipboard and clearing the selection, schematic save/load/list/paste across a
plugin restart, gzip-NBT round-trip and rejection of a non-gzip/truncated file,
brush application through the bulk path with undo, brush limit and region
denial, a mask-limited brush, empty-clipboard errors and an over-limit
clipboard refused before any edit starts.

The server suite covers the real service over the core
`BulkEdit`/journal (one revision, undo restores exactly), the core and
configured limits, region denial plus an operator bypass on a fill **and on an
undo over a region defined afterwards**, undo-handle scoping to its plugin, the
core coordinate parser, block resolution and hand lookup, and schematic files
surviving a new host. The tree suite drives `//name` through the real
dispatcher fallback.
