# CMD-44 — Plugin: social

Status: 📋 task brief (CMD0) · size: S · tier: WASM plugin (W) · plugin: `plugins/social`
Needs: `oc/P1`, `oc/CMD-32`, `oc/CMD-37` (teleport for `/tpparty`)

## Goal
Parties and friends for group play.

## Commands
| command | syntax | node |
|---|---|---|
| party | `/party create`, `/party invite <player>`, `/party accept\|deny`, `/party leave`, `/party kick <player>`, `/party disband`, `/party info` | `social.party` |
| party chat | `/party chat <message>` (`/pc`) | `social.party.chat` |
| tpparty | `/tpparty` / `/pt` (teleport the whole party to the leader/self) | `social.party.teleport` |
| friend | `/friend add <player>`, `/friend remove`, `/friend list`, `/friend requests`, `/friend accept` | `social.friend` |

Friendships are mutual (request/accept) and persist in player KV; a party is transient session state
with an owner, a member list and a chat channel. `/tpparty` uses the core teleport (CMD-37/31) and
respects region entry.

## Files
- `plugins/social/`: manifest, `src/`, README.
- player KV (P1) for friends; chat pipeline (CMD-40) for party chat; teleport (CMD-31/37).

## Edge cases to test explicitly
- inviting an already-invited/already-in-party player; inviting yourself.
- a party owner leaving/disbanding transfers or disbands per config; `/party kick` yourself.
- `/pc` only reaches online members; an offline member sees it on rejoin (or not; document).
- `/tpparty` with the leader offline; a member in a protected region.
- friend requests duplicate/expire; removing a friend removes both directions.
- `/friend list` pages for many friends.

## Done criteria
- Plugin + persistence + tests; README lists commands and links CMD-44.
- fmt / clippy `-D warnings` / `cargo-locked test`.
