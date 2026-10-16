# CMD-48 — One command catalogue: every command (working + pending), by category, in one maintained list

Read `docs/tasks/COMMON.md`, `docs/design/commands.md`, and the code it points at:
`crates/aether-server/src/commands/registry.rs`, `commands/tree.rs`, `commands/mod.rs`.

## Goal
Owners want **every** command of the project — those that work **and** those that do not yet — to be
visible as one catalogue, grouped by **category**, in the client (tab-completion / Declare Commands)
and in `/help`, with the not-yet-working ones **explicitly marked as not working**. Adding a new
command, or flipping one from "planned" to "working", must be a single edit in **one** list.

Today there are effectively two lists that can drift:
- `registry.rs::CORE` — only the **not-yet-implemented** commands (`name, aliases, node, op, task,
  group`); they are added to the tree as stubs (`tree.rs::stub`) and reply
  `"/<name> is not implemented yet (CMD-nn)"`.
- the **implemented** commands, split across `commands/core_nodes()`, `game::commands::nodes()`,
  `economy::commands::nodes()` — with **no** category, and no single listing.
- `registry.rs::help_lines()` hardcodes an `"Implemented: …"` string, which drifts as commands land.

## What to build
1. **One catalogue source of truth** (e.g. a new `commands/catalogue.rs`, or extend `registry.rs`)
   with one entry per command covering **both** working and pending:
   - `name`, `aliases`, `node`, `op`, `task` (`CMD-nn` / `-`), `category`, and
     `status: Implemented | Pending`.
   - Categories are the existing `group` values plus categories for the implemented verbs; agree on
     a small fixed set (e.g. `admin, player, world, data, ops, history, economy, plugin, …`) and use
     it as the `/help` grouping and in `docs/design/commands.md`.
   - The **implemented** entries must reference where their executor is built (the module's
     `nodes()`), so the catalogue is descriptive, and a test proves the two agree (see below). Keep
     the per-module `nodes()` builders; the catalogue is the **index/listing**, not a second
     implementation.
2. **Declare Commands (CMD-04) and the tree (CMD-03)** render from the catalogue: every catalogue
   name + alias is declared exactly once, implemented nodes wired to their real executor, pending
   ones to the stub that replies `not implemented yet (CMD-nn)`. A pending command IS still declared
   (so it autocompletes and colours) and still permission-gated by its `node`/`op`.
3. **`/help`** (built on CMD-05's tree-driven help) lists commands **by category**, and **marks the
   pending ones** as not implemented — pick one clear style and use it consistently, e.g. pending
   names shown with a trailing `(pending CMD-nn)` (or a distinct colour), implemented names plain;
   `/help <category>` lists one category; `/help <command>` gives the usage and, for a pending one,
   its task id. No client-visible lie: a player can tell what will not run.
4. **One edit to flip**: a command moving from planned to working changes its catalogue entry's
   `status` (and, where relevant, nothing else) — no second list, no hardcoded "Implemented:" string.
   Do **not** use commented-out code as the mechanism (rustfmt/tests make that fragile); a `status`
   field is the maintained single list. Explain the rule in the module doc.

## Constraints
- Vanilla behaviour: command names/aliases/op levels/nodes must still match `docs/design/commands.md`
  and the vanilla 1.21.11 command set; do not invent aliases.
- A plugin still may not take a core name (`registry::core_owns`), pending or working.
- Older experience levels: keep the existing `minecraft.command.*` / `aether.command.*` node naming.

## Edge cases to test explicitly
- every catalogue name **and** alias is declared exactly once in the wire tree (no duplicates, no
  missing) and answered by exactly one dispatcher;
- an implemented entry has a real executor; a pending entry replies `not implemented yet (CMD-nn)`;
- a name that is both "implemented" and listed pending fails the build/test (the drift guard);
- `/help` shows every category and marks every pending command; `/help <category>` filters;
- a non-op's `/help` does not leak op-only command names (or does, per vanilla — state which and
  match vanilla);
- aliases resolve to the canonical entry in `/help <alias>` and in completion.

## Files
- `crates/aether-server/src/commands/registry.rs` (+ maybe new `commands/catalogue.rs`).
- `crates/aether-server/src/commands/tree.rs` (`build`, `stub`), `commands/mod.rs` (`help_lines`,
  `RESERVED_COMMANDS`), and the `nodes()` in `game/commands.rs`, `economy/commands.rs`,
  `commands/core_nodes()`.
- `docs/design/commands.md` — add the category table and the "one list" rule.

## Done criteria
- One catalogue lists every command with a category and status; the tree, Declare Commands and
  `/help` are generated from it.
- Drift-guard tests above; `/help` marks pending commands.
- fmt / clippy `-D warnings` / `cargo-locked test` clean.
- A short note in `docs/design/commands.md` on how to add a command and how to flip it to working.
