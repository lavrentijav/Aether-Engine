# CMD-45 — Plugin: integrations

Status: 📋 task brief (CMD0) · size: M · tier: WASM plugin (W) · plugin: `plugins/integrations`
Needs: `oc/P1`, `oc/PA3` (net capability), `oc/CMD-32`

## Goal
Server-owner integrations: Discord chat bridge, vote rewards (NuVotifier), account linking and web
stats. All use the outbound HTTP/net capability (PA3) and player chat/events — never block the tick.

## Commands
| command | syntax | node |
|---|---|---|
| discord | `/discord [<message>]` (relay to the configured webhook; bridge chat both ways) | `integrations.discord` |
| vote | `/vote` (list vote sites), `/vote rewards`, `/votetop` | `integrations.vote` |
| link | `/link [<code>]`, `/unlink` | `integrations.link` |
| stats | `/webstats` (post a heartbeat/stats to the web endpoint) | `integrations.webstats` |

- **NuVotifier**: implement the v1/v2 vote listener (RSA-signed payload from the vote site) or the
  modern HTTP POST endpoint; on a valid vote, run the configured reward (economy/item/command).
- **Discord bridge**: an outbound webhook for chat, and (optionally) an inbound bot connection;
  messages are rate-limited and sanitised; a Discord message is attributed to a fixed name.
- **Link**: issue a one-time code the player enters on the website; store `uuid ↔ external id` in
  KV. Never trust an unverified external id.
- Web stats: periodic POST with TPS/players/version; failures are logged, never retried in a tight
  loop.

## Files
- `plugins/integrations/`: manifest, `src/`, README, example config.
- PA3 net capability (host HTTP client, allow-list, timeouts, scheduler callback); player KV.

## Edge cases to test explicitly
- an invalid/mismatched NuVotifier signature is rejected; a replayed vote is deduped.
- the vote listener is offline: no blocking; the plugin logs and retries with backoff.
- Discord webhook 429/5xx: messages are dropped or queued with a bounded buffer, not lost silently.
- `/link` code expiry and single use; a code for a player who already linked errors.
- inbound Discord text cannot inject a command (no leading `/` execution) or JSON component.
- `/discord` from console with no configured webhook errors clearly.
- the net capability is disabled: every command degrades with a clear message.

## Done criteria
- Plugin + a local mock endpoint for tests (no live internet in CI); README lists commands and links
  CMD-45.
- fmt / clippy `-D warnings` / `cargo-locked test`.
