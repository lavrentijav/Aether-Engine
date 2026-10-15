# CMD-07 — Console and RCON command sources

Status: 📋 task brief (CMD0) · size: M · core: yes
Needs: `oc/CMD-03`, `oc/CMD-01`

## Goal
Let commands run from a **server console** and over **RCON**, through the same tree/dispatch as
players. RCON is the standard remote-admin protocol; the catalogue makes it optional and off by
default.

## Console
- Read stdin lines; a line without `/` is a command in vanilla console (the console types commands
  without a slash). `stop`, `save-all`, etc. must work here.
- The console source has op level 4, no entity, no position; commands that need a position
  (`/setblock ~ ~ ~`) fail the way vanilla does ("That position is not loaded" / no entity).
- Console feedback is printed to stdout via the existing logger.

## RCON
- Implement the vanilla RCON protocol: TCP, packet types `AUTH` (3), `AUTH_RESPONSE` (2),
  `EXECCOMMAND` (2), `RESPONSE_VALUE` (0), `SIZE` (varint little-endian framed with trailing two
  NULs). Password from config (`[rcon] enabled=false, address, password, port=25575`).
- Off by default; refuse to start if enabled with an empty password. Bind loopback by default like
  the game port; log a warning if exposed (mirror `warn_if_exposed`).
- Auth failures are rate-limited and logged; a connection that never authenticates times out.
- A command's multi-line reply is sent as one RESPONSE_VALUE body (vanilla concatenates lines).

## Files
- new `crates/aether-server/src/commands/console.rs`, `crates/aether-server/src/rcon.rs`.
- `crates/aether-server/src/config.rs`: `[rcon]` section + sample config + env override.
- `crates/aether-server/src/main.rs`: spawn the stdin reader and, if enabled, the RCON listener.

## Edge cases to test explicitly
- RCON auth success/failure, wrong packet id, oversized packet, fragmented reads, multiple
  commands over one connection.
- a command that produces several lines arrives as a single response.
- `/stop` over RCON shuts the server down cleanly (same path as console).
- RCON disabled by default; enabling without a password is a startup error.
- an unknown command over console/RCON returns the same error a player gets.
- the stdin reader exits cleanly at EOF (piped input) without spinning.

## Done criteria
- Protocol-level tests against a raw socket client (the repo already tests 1.8.9 framing this way).
- Console tests via an injected reader/writer.
- fmt / clippy `-D warnings` / `cargo-locked test`.
