# CMD-04 — Declare Commands packet for every codec

Status: 📋 task brief (CMD0) · size: M · tier: core (C)
Needs: `oc/CMD-03`

## Goal
Send a faithful **Declare Commands** (a.k.a. command tree) packet built from the CMD-03 tree, for
**every supported protocol** (1.8.9…1.21.11 and 26.x that get the packet), so the client completes
every command, colours valid ones, and knows argument boundaries. Today `protocol/commands.rs`
sends a deliberately shallow tree: each command is one literal + one greedy string. Replace it with
the real tree.

## Version differences (check each codec, do not assume)
- Packet id and registration phase: pre-1.13 has no command tree (the client knows the vanilla
  command list itself) — send nothing there; document that.
- 1.13–1.18: node array, root index, parser names **as strings** (`brigadier:string`, …), flags.
- 1.19+: parser ids are **indices into a registry** (`Parsers::ByName` vs `Parsers::ById` already
  exists in `protocol/commands.rs`); argument/properties serializers differ per argument type.
- Node flags: `0x01` literal / `0x02` argument / `0x03` mask / `0x04` executable / `0x08` redirect /
  `0x10` suggestion-provider; `has_command`/`has_children` booleans in older forms.
- The packet must fit the frame; large trees use the normal length prefix — no special handling,
  but measure it (a 200-node tree is a few KB).
- Configure phase: from 1.20.2 the registries go in configuration, and the command tree is sent in
  Play; follow each existing codec's placement.

## Files
- `crates/aether-server/src/protocol/commands.rs` (rewrite the serializer; keep the tests).
- each codec's join/handshake path that currently calls `write_tree` (grep `write_tree`).
- `commands/tree.rs`: expose a serialization view (nodes + children + parser + executor flag).

## Edge cases to test explicitly
- every declared node round-trips through a decode helper for `ByName` and `ById` codecs.
- no command name is declared twice; aliases are declared and point at the same executor.
- a command with no arguments is executable on its own; one with arguments is executable both bare
  and with args where vanilla allows.
- argument parser id/name matches the codec's `minecraft-data` registry for that release.
- a codec that must not send the packet sends nothing (test asserts absence).
- requirement-gated nodes are still declared (the client decides visibility from the tree); the
  server enforces on execution.

## Done criteria
- A test that decodes the tree for each parser mode and asserts structure/uniqueness.
- A test that every name the tree declares is actually answered by the dispatcher (the existing
  `every_declared_command_is_answered` idea, now generated from the tree and including the CMD0b
  stub registry).
- fmt / clippy `-D warnings` / `cargo-locked test` for the server and net crates.
