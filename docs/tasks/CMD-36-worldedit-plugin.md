# CMD-36 — Plugin: WorldEdit-style editing

Status: 📋 task brief (CMD0) · size: L → split · core: no · plugin: `plugins/worldedit`
Needs: `oc/P1`, `oc/CMD-30`, `oc/CMD-06`

## Goal
Selection, clipboard, brushes and schematics in the familiar `//` command style, on top of the core
bulk edit API (CMD-30). The core owns the batched writes and journal; this plugin owns selection
state, the clipboard, operations and undo history (`docs/design/commands.md` §3).

**Split before assigning**: this is an L. Suggested split:
- **CMD-36a selection + set/replace/walls/outline/undo/redo/expand/contract** (first session).
- **CMD-36b copy/cut/paste/rotate/flip/stack/move/schem/brushes** (second session).
This brief is the combined spec; create `CMD-36a`/`CMD-36b` briefs when splitting.

## Commands
| command | syntax | node |
|---|---|---|
| wand | `//wand` (give a wooden axe) | `worldedit.wand` |
| selection | `//pos1 [<pos>]`, `//pos2 [<pos>]`, `//sel`, `//expand <n> [dir]`, `//contract <n> [dir]` | `worldedit.selection` |
| fill | `//set <pattern>`, `//replace [<filter>] <pattern>`, `//walls <pattern>`, `//outline <pattern>` | `worldedit.region.*` |
| clipboard | `//copy`, `//cut`, `//paste [-a] [-o] [-s]`, `//rotate <y> [x] [z]`, `//flip [dir]`, `//stack <n> [dir]` | `worldedit.clipboard.*` |
| move | `//move <n> [dir]`, `//move <dir> <n>` | `worldedit.region.move` |
| history | `//undo [<n>]`, `//redo [<n>]` | `worldedit.history.*` |
| schematics | `//schem save\|load\|list\|paste <name>` | `worldedit.schematic.*` |
| brushes | `//brush sphere\|cyl\|smooth\|...`, `/brush`, `/mask` | `worldedit.brush.*` |

Patterns: `stone`, `50%stone,50:dirt`, `#clipboard`, `#hand`, `#pos1`, `#existing`. Masks in
`//replace [mask]`. Limits: a configurable max edit volume and a per-tick budget via CMD-30; a
volume over the limit errors before starting.

## Files
- `plugins/worldedit/`: manifest, `src/`, README.
- core bulk edit (CMD-30), selector/coordinate parser (CMD-06), region check (CMD-29) so a brush
  cannot edit a protected region without bypass.

## Edge cases to test explicitly
- `//set` on a selection over the max volume is refused; a legal one commits atomically.
- patterns `50%`, `#clipboard`, `#hand`; an invalid pattern errors.
- `//copy` then `//paste` at the same place; paste with `-a` skips air, `-o` overwrites, `-s` selects.
- `//rotate 90` then `//paste`; `//flip` twice returns the original.
- `//undo` after `//set` restores exactly; history depth bounded; `//redo` after undo.
- editing inside a claim/region without bypass is denied.
- `//schem save` writes a `.schem`-compatible file; `load`+`paste` round-trips.
- `//wand` on an older codec (item + block-click interaction) still selects.
- a very large `//stack` is spread over ticks and does not stall.

## Done criteria
- Plugin + tests; README lists commands and links CMD-36 (and split ids).
- fmt / clippy `-D warnings` / `cargo-locked test`.
