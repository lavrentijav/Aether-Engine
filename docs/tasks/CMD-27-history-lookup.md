# CMD-27 — History: lookup filters, `/co`, preview

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`

## Goal
Extend the existing history commands (`commands/mod.rs`, `commands/query.rs`) with the
CoreProtect-style filter grammar and root, so an operator migrating from CoreProtect can type the
commands they know. `/inspect`, `/lookup`, `/rollback` already exist — this task adds syntax, not a
new journal.

## Commands
| syntax | node | op | behaviour |
|---|---|---|---|
| `/co inspect` (alias `/co i`) | `aether.command.inspect` | 2 | same as existing `/inspect` |
| `/co lookup <filters>` (alias `/co l`) | `aether.command.lookup` | 2 | same as existing `/lookup`, extended filters |
| `/co rollback <filters>` (alias `/co rb`) | `aether.command.rollback` | 3 | same as `/rollback` |
| `/co restore <filters>` | `aether.command.restore` | 3 | implemented in CMD-28 |
| `/co purge <filters>` | `aether.command.purge` | 4 | implemented in CMD-28 |
| `/co help` | `aether.command.lookup` | 0 | usage |

Filters to add to `query.rs` (existing: `player:`/`u:`, `time:`/`t:`, `radius:`/`r:`, `block:`,
`action:`/`a:`):
- `include:` / `i:` and `exclude:` / `e:` block lists (comma-separated, `ns:id` or tag `#tag`).
- `count:` / `#` maximum rows (default 500 cap).
- `world:` / `dimension:` (once multi-world, CMD-26).
- `order:` oldest/newest (CoreProtect `#` and time order).
- `page:` for the pager.
- radius supports `r:#global` for no radius limit (CoreProtect).

`/lookup` must reject an unknown/mistyped filter exactly as today (`ParseError::UnknownKey`), never
silently ignore it. Preview: `/co rollout`-style preview is not CoreProtect; add a `preview:` flag
to `/co rollback` that shows the blocks that *would* change (counts per block, first N) and asks for
`confirm` before applying, or implement `/co preview <filters>` as a read-only view. Decide and
document.

## Files
- `crates/aether-server/src/commands/query.rs` (filters), `commands/mod.rs` (`/co` router).
- `crates/aether-server/src/commands/pager.rs` (paging the bigger result sets).

## Edge cases to test explicitly
- aliases `u/t/r/a/b/i/e/#` map to the same filters as the long names.
- `count:` caps the result and the header says `showing <n> of <m>`.
- include/exclude with a block tag; an unknown tag errors.
- `r:#global` with no radius; a huge radius does not scan the whole world (bounded reads).
- combining `u:steve a:break` is AND (existing behaviour).
- a filter value with a colon inside (`block:minecraft:stone`) parses correctly.
- preview makes no journal entry and changes no block.

## Done criteria
- Extended parser + tests; `/co <sub>` routes to the right handler.
- fmt / clippy `-D warnings` / `cargo-locked test`.
