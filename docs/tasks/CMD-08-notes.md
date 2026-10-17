# CMD-08 — implementation notes

Status: ✅ implemented on `oc/CMD08`.

## What landed

- `crates/aether-server/src/admin.rs`: `/op`, `/deop`, `/ban`, `/ban-ip`,
  `/banlist [ips|players]`, `/pardon`, `/pardon-ip`, `/kick`, `/whitelist
  <on|off|list|add|remove|reload>`, all at vanilla op level 3 through the
  CMD-01 node/op gate (`minecraft.command.*`).
- Target lists use the CMD-06b selector parser for `@…` and accept a plain
  player name where vanilla takes a game profile. An empty selector and a
  `kick` of an offline name are `No player was found`, not a silent skip.
- Bans and the whitelist are sidecar JSON files under `[server] world_dir`:
  `banned.json` (player and IP entries with reason/source) and
  `whitelist.json` (`enabled` plus name/UUID rows). Writes are atomic
  (temp file + rename). They are files, not world-KV keys, so no T5 key prefix
  is claimed; `/whitelist reload` re-reads the file.
- `session::login_and_play` calls `admin::admit` before authentication (by
  claimed name/offline UUID) and again after it (by the authenticated UUID),
  next to the max-players/operator admission. An address ban and a name ban are
  both checked; operators bypass the whitelist but never a ban.

## Decisions

- **Operators:** the CMD-01 store is authoritative; `config.operators` is only
  the add-only startup seed. `/op name` adds a bare member to the `op` group.
  An IP-bound seed entry (`name@address`) is **not widened**: the store's
  `add_member` preserves an existing binding, so `/op name` reports
  `Nothing changed` and the binding stays. `/deop name` removes the member
  whole, bound or not.
- **Whitelist and the legacy config:** `config.whitelist` seeds
  `whitelist.json` only when the file does not exist yet (enabled iff the list
  is non-empty). Once the file exists it is the source of truth, like
  `permissions.toml` after its seed.
- **Whitelist UUIDs:** an offline target stores `offline_uuid(name)`; an
  online target stores its live UUID. A login matches either the name
  (case-insensitively) or the UUID, so a later profile with the same UUID is
  admitted.
- **Reasons:** `/ban`/`/ban-ip` without a reason store vanilla's
  `Banned by an operator.`; the success line still follows the brief
  (`Banned <player>` / `Banned IP address <ip>`). A kick without a reason uses
  `Kicked by an operator`. `/pardon` of an unbanned name is an **error**
  (`Nothing changed…`), carried by the new `Ctx::fail` path so feedback keeps
  the success/error distinction.
- **Online effect:** banning or kicking a connected target disconnects that
  handle; the disconnect reason is the ban/kick text. Whitelist changes do not
  kick anyone already online (vanilla does not either).

## Review 1 fixes

- Whitelist is now a **post-authentication** gate: `session::login_and_play`
  calls `admin::ban_admission` (name + address) before auth and
  `admin::admit` (ban + whitelist by the real UUID) after it. A whitelist row
  keyed only by UUID admits a renamed premium player.
- A corrupt `banned.json` or `whitelist.json` now **refuses startup** with a
  clear error and is left untouched; it is never replaced by an empty list or
  by a disabled whitelist.
- `/ban-ip <literal ip>` kicks every connected handle whose (canonicalised)
  address matches.
- Repeated operations use the vanilla `en_us` failures (`commands.op.failed`,
  `deop.failed`, `ban.failed`, `banip.failed`, `pardon.failed`,
  `pardonip.failed`, `whitelist.alreadyOn/Off`, `whitelist.add/remove.failed`).
  `/banlist [ips|players]` uses the one `There are N bans:` header, and
  `/kick` always includes the reason (default `Kicked by an operator`).
- The tree mirrors the vanilla grammar: `game_profile` targets for
  op/deop/ban/pardon/whitelist add/remove, a word for ban-ip/pardon-ip,
  `message` for reasons, `entity` for kick, and literal subcommands for
  banlist and whitelist. An extra argument is a parse error.
- Entity/profile arguments read one selector-aware token, so
  `@a[name="Bob Smith"]` stays one argument; reasons keep their spaces.
- `::ffff:a.b.c.d` is canonicalised to IPv4 when bans are stored and compared.
- `warn_if_exposed` reads the live `whitelist.json` state
  (`admin::whitelist_enabled`); the unused `Config::may_join` is gone.
- `docs/COMMANDS.md` / `.ru` are regenerated (usage escaped for the markdown
  table), and `KNOWN_ISSUES.ru.md` / `STATUS.ru.md` are updated.
