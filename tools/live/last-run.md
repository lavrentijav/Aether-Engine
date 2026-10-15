# V1 live run

- Date: 2026-10-15 21:49 UTC
- Target: fi2.walk.boykisser.online:25565, account **`_aether_bot1`**
  (operator, offline mode)
- Server: fi2, deployed 2026-10-15 18:29:54 UTC, binary sha256
  `5c51b0cc4d4639b6…`, main `a8545f6` (AU2); `[auth] mode = "hybrid"`,
  operators `_alpha_01,_aether_bot1@144.31.61.39` (`AETHER_OPERATORS`)
- Versions: 1.21.11 1.21.4 1.20.6 1.20.4 1.20.2
- Harness: `tools/live/run.sh` + `all.sh` (MCC 26.2) and `auth.sh` (raw login
  probe); scratch in `/tmp/opencode/live`.

## Live suite (10 scenarios × 5 versions)

| scenario | 1.21.11 | 1.21.4 | 1.20.6 | 1.20.4 | 1.20.2 |
| --- | --- | --- | --- | --- | --- |
| join | PASS | PASS | PASS | PASS | PASS |
| inspect | PASS | PASS | PASS | PASS | PASS |
| place-break-rollback | PASS | PASS | PASS | PASS | PASS |
| inventory-persist | PASS¹ | PASS | PASS | PASS | PASS |
| eat-effects | PASS | PASS | PASS | PASS | PASS |
| container | PASS | PASS | PASS | PASS | PASS |
| potion | PASS | PASS | PASS | PASS | PASS |
| commands | PASS | PASS | PASS | PASS | PASS |
| journal-upgrade | PASS | PASS | PASS | PASS | PASS |
| multi-version-join | PASS | PASS | PASS | PASS | PASS |

50/50 pass. ¹ The `inventory-persist 1.21.11` run in the matrix was aborted by
a transient out-of-disk on the harness host (not the server):

```
sed: couldn't flush stdout: No space left on device
run.sh: line 68: 1107585 Aborted   timeout "$TOTAL" "$MCC" ...
```

The root filesystem was momentarily full (another build); it had 25 GB free
moments later and the same scenario re-ran **PASS**. No other run was affected.

## Auth (AU1 hybrid, 1.21.11)

```
PASS auth bot-name-not-premium _aether_bot1 has no Mojang profile (HTTP 404)
PASS auth invalid-name-space refused: {"text":"Invalid username."}
PASS auth invalid-name-long refused: {"text":"Invalid username."}
PASS auth invalid-name-short refused: {"text":"Invalid username."}
PASS auth login-rate-limit admitted=10 then refused: refused: {"text":"Too many login attempts; try again in a minute."}
PASS auth login-rate-recovery admitted after 62s
PASS auth offline-open admitted	id=0x03
PASS auth bot-admitted PASS join 1.21.11
```

- The bot name is not premium (404), so the server never offers it encryption.
- Invalid names (`bad name`, 20-char, `ab`) are refused with `Invalid username.`
- Rapid reconnects hit the per-IP limit (`login_rate_per_min = 10`): 10
  admitted, then refused; recovery after the 60 s window.
- Offline play stays open; the bot joins via MCC.

## Account / config change (this run)

Tests run as `_aether_bot1` (no Mojang profile), replacing the owner's real
premium account `_alpha_01`. Operator rights come from the fi2 server config:
`AETHER_OPERATORS=_alpha_01,_aether_bot1@144.31.61.39` in `/etc/aether.env`
(bound to this harness's source IP `144.31.61.39`). The MCC AutoRelog
workaround needed for the premium account is removed.

### `inventory-persist` made deterministic

The first version of this scenario read the *held hotbar* slot through MCC's
local inventory cache right after a creative slot write; the cache lags the
server, so it was flaky (5/5 failures in one matrix run, then 4/4 passes
standalone). It now writes a fixed main-inventory slot (9) with a distinctive
item (`nether_star`) and asserts on the server's authoritative view — the
`/inv` history command — before and after `/reco`:

```
send /inventory creativedelete 9
send /inventory creativegive 9 NetherStar 7
send /inv                      -> "slot 9: 7x minecraft:nether_star"
send /reco
send /inv                      -> "slot 9: 7x minecraft:nether_star"
```

## Coordinate note

The bot's world spawn is `(56.5, 63, 8.5)` (the owner's account had a personal
spawn near `-8 70 8`), so `inspect`, `place-break-rollback`, `container` and
`journal-upgrade` anchor on nearby cells. `journal-upgrade` still reads the old
v3 region at `-8 69 8` (the owner's pre-upgrade edits) alongside a fresh v4
block, so the v3+v4 read path is still covered.

## How to reproduce

```sh
export PATH="$HOME/.cargo/bin:/srv/dev/tools/bin:$PATH"
cd tools/live
bash all.sh          # the 10×5 client suite, then auth.sh -> last-run.md
bash auth.sh         # auth scenario alone (~2.5 min: waits out two 60 s windows)
```
