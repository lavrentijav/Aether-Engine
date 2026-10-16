# Chat, private-message and command logging (CH1)

Status: ✅ implemented (2026-10-16). Core, small. Config is `[chat]`; the line and event formats
below are stable. Journal format **v5** added the player-text body.

## What is logged

Every line a player types is written twice:

1. **The server log**, one of three grep-friendly prefixes:
   - `[chat] <name> text` — a public chat line.
   - `[pm] name -> target: text` — a private message (`/msg`, `/tell`, `/w`).
   - `[cmd] name: /command line` — a command, slash included.
2. **The world journal**, as an `EventBody::ChatMessage` carrying the actor, the wall-clock time,
   where the speaker stood and the text. The history API (WEB/PA2) and the web admin read the
   journal, not the log file, which is why both exist.

A command that is vetoed by a plugin (`before-chat`), or a chat line that is vetoed, is still
logged as an event; only a chat line that is actually delivered is logged as `[chat]`. A command is
logged before it runs, so the audit trail records the attempt even when the command is refused.

## Config

```toml
[chat]
log = true           # public chat and private messages
log_commands = true  # commands players type
redact = ["login", "register", "l", "reg"]
```

Both switches default to `true`. `redact` is a case-insensitive list of command names (without the
leading slash) whose **arguments** are replaced with `****` before either the log line or the
journal event is written — the verb is kept, because an audit trail should say a login happened.
The raw line is dropped, never stored. A command the list does not name is written verbatim.

Redaction is per *command name*, not a content scan: a secret typed as an argument to an ordinary
command (`/msg Bob my password is …`) is still logged, as it is in every server's console. The
default list covers the common authentication plugins; an operator running a different one adds its
names.

## The journal event

`FORMAT_VERSION` is now **5**. Tags 7–9 are the three player-text kinds, one tag each so the
history mirror's `kind` column tells them apart without reading the body:

| tag | kind | peer | text |
|---|---|---|---|
| 7 | public chat | `ActorId::SERVER` | the message |
| 8 | private message | the recipient | the message |
| 9 | command | `ActorId::SERVER` | the (redacted) line, slash included |

The body also carries the speaker's block position, so a radius filter (`/lookup radius:20`) finds
what was said nearby. A chat event is not a world change: rollback ignores it (`plan_rollback` acts
on `BlockSet` only), and the item ledger folds it in as a no-op. Older (v1–v4) history still
decodes.

## Private messages

`/msg <player> <message>` and its aliases `/tell` and `/w` are core commands (owned by the core
permanently; a plugin may only hook them). The sender sees `You whisper to <player>: <message>`,
the recipient `<sender> whispers to you: <message>`. A target that is not an online player is
refused; targeting yourself is allowed, as in vanilla. The full `/msg` command group (selectors,
`tellraw`-style components) is CMD-17a; this is the minimal form CH1 needs to log.

## The PostgreSQL mirror

The `aether_events` table gained `text TEXT` and `peer UUID` (added with
`ADD COLUMN IF NOT EXISTS`, so a live mirror heals on reconnect). `kind` is the tag above;
`aether_events_named` shows the resolved kind name. Everything else — item, block, provenance —
follows the existing columns.

## Known limits

- Chat shares the world journal, so `/lookup` and `/inspect` can return chat events alongside block
  changes. They render as `who said: …` / `who -> who: …` / `who ran: …`.
- Console and RCON commands are not logged here yet; CH1 is scoped to lines typed by players.
- The default `redact` list is the brief's (`login`, `register`, `l`, `reg`), but `l` is also the
  core `/lookup` alias on this server, so a `/l player:…` query is logged with its filters masked.
  An operator who uses `/l` for history should drop `l` from the list (passwords are still covered
  by `login`/`register`/`reg`).
- The position is where the speaker stood when the line was processed, not exactly when it was
  typed; it is block-level and within a tick.
