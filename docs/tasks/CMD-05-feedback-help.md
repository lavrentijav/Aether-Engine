# CMD-05 — Command feedback, `/help`, aliases, `sendCommandFeedback`

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-03`

## Goal
Make command output behave like vanilla: a single feedback path, translatable messages with
placeholders, the gamerule `sendCommandFeedback` respected, `/help` generated from the command
tree (with paging), and alias handling defined centrally. CMD0b already added a minimal
`/help` over the stub registry; this task replaces it with the tree-driven one.

## Behaviour
- **Feedback** goes to the command source only by default; `/execute ... run` and `/say` broadcast
  where vanilla broadcasts. Introduce `CommandSource` (player / console / RCON / command block /
  function) so the same executor works for all (console/RCON is CMD-07).
- **`sendCommandFeedback`** (default `true`): when off, command success/failure feedback is
  suppressed for command blocks and functions but **not** for players/console — match vanilla
  exactly (players always see their own feedback).
- **`/help [command]`**: no arg → list all commands the caller may run, paged (reuse
  `commands::pager`); with a command → usage string built from the tree (vanilla
  "/gamemode <gamemode> [<targets>]"). Unknown command → "Unknown command. Try /help".
- **Aliases**: declared once on the literal node; both name and alias appear in `/help` and the
  Declare Commands packet (CMD-04). No alias may collide with any literal.
- **Messages**: use vanilla's 1.21.11 `en_us` strings for command success/errors where they exist
  (`commands.give.success.single`, `commands.teleport.success.location.single`, …). A found string
  is copied verbatim; a missing one is added to a small `lang` table with a `// TODO verify` note.
- Red/white styling: errors as vanilla (red for failure), success plain; do not invent colour.

## Files
- `crates/aether-server/src/commands/mod.rs` + new `commands/feedback.rs`, `commands/source.rs`.
- `crates/aether-server/src/commands/pager.rs` (reuse).
- `crates/aether-server/src/protocol` for the `sendCommandFeedback` gamerule if it is not in
  CMD-13's `gamerule` implementation yet.

## Edge cases to test explicitly
- `/help` with no permission filtering leak: a non-op must not see op-only commands.
- `/help <alias>` resolves to the canonical usage.
- `sendCommandFeedback=false`: feedback suppressed for a function/command block, still shown to a
  player.
- an empty page, first page, last page, and `goto` out of range.
- a command that succeeds with no message emits nothing (e.g. `/kill` self, already the case).
- an unknown command still produces exactly one line.

## Done criteria
- `/help` renders from the tree; stub entries from CMD0b are listed and marked as not implemented.
- Unit tests for the edge cases; a snapshot test per vanilla message used.
- fmt / clippy `-D warnings` / `cargo-locked test`.
