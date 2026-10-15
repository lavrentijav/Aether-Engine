# CMD-36b — Plugin: WorldEdit clipboard, schematics, brushes

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/worldedit`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-30b`, `oc/CMD-36a`
Parent/umbrella: `docs/tasks/CMD-36-worldedit-plugin.md`

## Commands
| command | syntax | node |
|---|---|---|
| clipboard | `//copy`, `//cut`, `//paste [-a] [-o] [-s]` | `worldedit.clipboard.*` |
| transform | `//rotate <y> [x] [z]`, `//flip [dir]`, `//stack <n> [dir]` | `worldedit.clipboard.*` |
| move | `//move <n> [dir]`, `//move <dir> <n>` | `worldedit.region.move` |
| schematics | `//schem save\|load\|list\|paste <name>` | `worldedit.schematic.*` |
| brushes | `//brush sphere\|cyl\|smooth\|…`, `/brush`, `/mask` | `worldedit.brush.*` |

## Files
- `plugins/worldedit/`: manifest, `src/`, README.
- selection + history from CMD-36a; core bulk edit (CMD-30).

## Edge cases to test explicitly
- `//copy` then `//paste` at the same place; `-a` skips air, `-o` overwrites, `-s` selects result.
- `//rotate 90` then `//paste`; `//flip` twice returns the original.
- `//schem save` writes a `.schem`-compatible file; `load`+`paste` round-trips.
- a very large `//stack` is spread over ticks and does not stall.
- a brush inside a protected region without bypass is denied.

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
