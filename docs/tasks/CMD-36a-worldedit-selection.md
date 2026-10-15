# CMD-36a — Plugin: WorldEdit selection & fill

Status: 📋 task brief (CMD0) · size: M · core: no · plugin: `plugins/worldedit`
Needs: `oc/P1`, `oc/CMD-30b`, `oc/CMD-06`
Parent/umbrella: `docs/tasks/CMD-36-worldedit-plugin.md`

## Commands
| command | syntax | node |
|---|---|---|
| wand | `//wand` | `worldedit.wand` |
| selection | `//pos1 [<pos>]`, `//pos2 [<pos>]`, `//sel`, `//expand <n> [dir]`, `//contract <n> [dir]` | `worldedit.selection` |
| fill | `//set <pattern>`, `//replace [<filter>] <pattern>`, `//walls <pattern>`, `//outline <pattern>` | `worldedit.region.*` |
| history | `//undo [<n>]`, `//redo [<n>]` | `worldedit.history.*` |

Patterns: `stone`, `50%stone,50:dirt`, `#clipboard`, `#hand`, `#pos1`, `#existing`. A configurable
max edit volume and per-tick budget come from the core bulk-edit API (CMD-30).

## Files
- `plugins/worldedit/`: manifest, `src/`, README.
- core bulk edit (CMD-30), selector/coordinate parser (CMD-06), region check (CMD-29) so a fill
  cannot edit a protected region without bypass.

## Edge cases to test explicitly
- `//set` over the max volume is refused before starting; a legal one commits atomically.
- patterns `50%`, `#hand`; an invalid pattern errors.
- `//undo` after `//set` restores exactly; history depth bounded; `//redo` after undo.
- editing inside a claim/region without bypass is denied; `//wand` on an older codec still selects.

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
