# WorldEdit plugin — skeleton (CMD-36)

Status: 📋 skeleton (CMD0), **native tier** (in-process, CMD-00N). Needs the native loader (`oc/CMD-00N`), the core bulk-edit API (`oc/CMD-30`)
and the selector/coordinate parser (`oc/CMD-06`). **No code until P1 is merged** — manifest and
README only. Size L: split into `CMD-36a` (selection + fill + undo) and `CMD-36b` (clipboard,
schematic, brushes) before assigning.

- CMD task: **CMD-36** — `docs/tasks/CMD-36-worldedit-plugin.md`
- Reference: WorldEdit. The batched writes are core; this plugin owns selection, clipboard and
  operations.

## Commands
| command | syntax | permission |
|---|---|---|
| `//wand` | give the selection wand | `worldedit.wand` |
| `//pos1` / `//pos2` | set selection corners | `worldedit.selection` |
| `//set <pattern>` / `//replace [<filter>] <pattern>` | | `worldedit.region.*` |
| `//walls <pattern>` / `//outline <pattern>` | | `worldedit.region.*` |
| `//expand <n> [dir]` / `//contract <n> [dir]` | | `worldedit.selection` |
| `//copy` / `//cut` / `//paste` | `-a -o -s` | `worldedit.clipboard.*` |
| `//rotate <y> [x] [z]` / `//flip [dir]` / `//stack <n> [dir]` | | `worldedit.clipboard.*` |
| `//move <n> [dir]` | | `worldedit.region.move` |
| `//undo [<n>]` / `//redo [<n>]` | | `worldedit.history.*` |
| `//schem save\|load\|list\|paste <name>` | | `worldedit.schematic.*` |
| `//brush …` / `/brush` / `/mask` | | `worldedit.brush.*` |

## Capabilities / events
`log`, `config`, `supports`, `storage.kv`, `world.read`, `world.write`, `players`. Subscribes to
`on-command`, `before-use-item` (wand/brush).

## Notes
Command names are declared to the host without the `//` prefix. A configurable max edit volume and
per-tick budget come from the core bulk-edit API.
