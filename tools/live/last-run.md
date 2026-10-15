# V1 live run

- Date: 2026-10-15 11:31 UTC
- Target: fi2.walk.boykisser.online:25565, account `_alpha_01` (operator, offline mode)
- Server: fi2 running main `2e7c918` (`aether.service` started 2026-10-15 10:33:46 UTC,
  binary sha256 `fa0451a6b17c0eae…`)
- Versions: 1.21.11 1.21.4 1.20.6 1.20.4 1.20.2
- Harness: `tools/live/run.sh` + `tools/live/all.sh`, MCC `26.2`, scratch in `/tmp/opencode/live`

## Live suite (10 scenarios × 5 versions = 50 runs)

| scenario | 1.21.11 | 1.21.4 | 1.20.6 | 1.20.4 | 1.20.2 |
| --- | --- | --- | --- | --- | --- |
| join | PASS | PASS | PASS | PASS | PASS |
| inspect | PASS | PASS | PASS | PASS | PASS |
| place-break-rollback | PASS | PASS | PASS | PASS | PASS |
| inventory-persist | PASS | PASS | PASS | PASS | PASS |
| eat-effects | PASS | PASS | PASS | PASS | PASS |
| container | PASS | PASS | PASS | PASS | PASS |
| potion | PASS | PASS | PASS | PASS | PASS |
| commands | PASS | PASS | PASS | PASS | PASS |
| journal-upgrade | PASS | PASS | PASS | PASS | PASS |
| multi-version-join | PASS | PASS | PASS | PASS | PASS |

50/50 pass. Two changes since the previous run (`2026-10-15 10:03 UTC`, server main
`20c1a3b`):

- **`eat-effects 1.20.2` now PASSES.** Main `2e7c918` merged `oc/V1`, so the server
  sends the legacy 1-based mob-effect id on protocol 764. The client now shows
  `Regeneration II` + `Absorption` (previously `Nausea II` + `Health Boost`). This
  is the live confirmation of the `b7c6d76` fix.
- **`commands` expectation updated for CMD0.** Main replaced the `/help` reply. The
  full run caught it (5 fails, one excerpt below); the scenario now asserts the
  catalog instead of the old `History commands:` blurb and re-passes on all five
  versions. Nothing about the game is broken — only the help text changed.

### The `commands` fail the full run caught (before the expectation update)

```
▌Time set to 1000
▌_alpha_01 is now in Creative mode
▌Teleported to 0 100 0
▌Teleported to spawn
▌Commands not implemented yet — /help <command> for the node and task:
▌  admin: op deop ban ban-ip banlist pardon pardon-ip kick whitelist
▌  ...
▌  history: co restore purge
▌Implemented: /help /inspect /lookup /rollback /page /stash /audit /cache
▌(86 commands pending, all runnable now)
```

The old server answered `/help` with `History commands: …`; CMD0 replaced it with
this task catalog, so `/send /help` legitimately no longer contains that string.

## Journal upgrade: v3 (before) + v4 (after) both readable

fi2 upgraded from a v3-journal build to the v4-journal build at `10:33:46 UTC`
without wiping the world, so the on-disk journal now holds entries written by both.
The `journal-upgrade` scenario checks `/inspect`, `/lookup` and `/audit` against both.

- **`/audit` decodes the whole journal** (it walks every event with no bound):
  `▌399 item instance(s), 133 anomal(y/ies):`. No `history unreadable` /
  `unsupported journal format`. The oldest events are seq 1, `2026-09-10`, i.e. far
  older than v3, so the reader accepts every historical format (v1–v4).
- **`/inspect` a pre-upgrade block** (`-8 69 8`, not modified since the upgrade;
  every entry `6h ago` = ~05:18 UTC, written by the *previous* build → v3):
  ```
  ▌-8 69 8 (now minecraft:grass_block[snowy=false]) — page 1 of 2 (11 entries)
  ▌  6h ago _alpha_01 placed minecraft:grass_block[snowy=false] at -8 69 8  #4245
  ▌  6h ago _alpha_01 broke minecraft:grass_block[snowy=false] at -8 69 8  #4244
  ▌  6h ago _alpha_01 placed minecraft:stone at -8 69 8  #4232
  ▌  ...
  ```
- **`/inspect` a block changed after the upgrade** (`-6 70 8`, freshly placed now →
  v4) returns the new entry alongside the older ones:
  ```
  ▌-6 70 8 (now minecraft:stone) — page 1 of 6 (57 entries)
  ▌  5s ago _alpha_01 placed minecraft:stone at -6 70 8   #12377
  ▌  24m ago _alpha_01 broke minecraft:stone at -6 70 8   #12328
  ...
  ```
- **`/lookup player:_alpha_01 radius:8`** returned a 101-entry page spanning the
  upgrade, again with no decode error.

(The `133 anomal(y/ies)` from `/audit` are the pre-existing `NeverMinted`/provenance
anomalies on very old uids, unrelated to format decoding; the decode itself is clean.)

## PostgreSQL mirror: `aether_events_named` returns readable rows

`tools/live/pg-check.sh` runs read-only SELECTs against the mirror on fi2
(connection string from `/etc/aether.env`, `dbname=aether_history`). The actor for
`_alpha_01` is `c1bb91ad-ab62-3f9f-8ab7-78f49746acf0`, confirmed by the newest
`block_set` at the just-written scenario coordinate `-6 70 8`.

```
SELECT actor, count(*) ... FROM aether_events_named
WHERE actor='c1bb91ad-ab62-3f9f-8ab7-78f49746acf0' GROUP BY actor;
```
```
                actor                 | events |           first            |            last
--------------------------------------+--------+----------------------------+----------------------------
 c1bb91ad-ab62-3f9f-8ab7-78f49746acf0 |   2654 | 2026-09-10 18:53:22.826+03 | 2026-10-15 13:41:48.859+03
```

Every `kind_name` resolves (no NULL join misses) and item names are resolved from
the uid's mint row; a sample of the newest rows:

```
 at             | seq   | kind_name   | x  | y | z | item_name        | item_count | reason_name      | place_to
----------------+-------+-------------+----+---+---+------------------+------------+------------------+-----------------------------------------
 13:41:48.859   | 12302 | block_set   | -6 | 70| 8 |                  |            |                  |
 13:41:42.874   | 12301 | item_mint   |    |   |   | minecraft:stone  | 64         |                  | inv:c1bb…:36
 13:41:41.874   | 12299 | item_destroy|    |   |   | minecraft:stone  |            | creative_replace | inv:c1bb…:36
 13:39:47.285   | 12290 | item_destroy|    |   |   | minecraft:potion |            | creative_replace | inv:c1bb…:36
 ...
```

Kind coverage for the session — all codes map to a name:

```
 kind_name    | count
--------------+-------
 block_set    |  2123
 item_destroy |   266
 item_mint    |   169
 item_derive  |    51
 item_move    |    44
 item_merge   |     1
```

`item_merge` (new in journal v4) is present, so the v4 events reach the mirror too.
Rows where `item_name` is NULL are exactly those with no `item_uid` (events that
carry no item identity); every row that has a uid resolves a name.

## How to reproduce

```sh
export PATH="$HOME/.cargo/bin:/srv/dev/tools/bin:$PATH"
cd tools/live
bash all.sh                      # the 10×5 client suite -> last-run.md
bash pg-check.sh c1bb91ad-ab62-3f9f-8ab7-78f49746acf0   # read-only mirror check
```

No cargo build is involved in the live suite; it only drives the prebuilt MCC
client at `/srv/dev/tools/mcc/MinecraftClient`.
