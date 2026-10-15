# V1 live run

- Date: 2026-10-15 19:39:48 UTC
- Target: fi2.walk.boykisser.online:25565, account **`_aether_bot1`**
  (operator, offline mode) — switched from the owner's real premium account
- Server: fi2, deployed 2026-10-15 18:29:54 UTC, binary sha256
  `5c51b0cc4d4639b6…`, main `a8545f6` (AU2: online-auth `should_authenticate`
  gate fix); `[auth] mode = "hybrid"`
- Versions: 1.21.11 1.21.4 1.20.6 1.20.4 1.20.2
- Harness: `tools/live/run.sh` + `all.sh` (MCC 26.2) and `auth.sh` (raw login
  probe); scratch in `/tmp/opencode/live`.

## Account change

Live tests now run as **`_aether_bot1`**, a dedicated bot name with **no**
Mojang profile (`api.mojang.com/users/profiles/minecraft/_aether_bot1` → HTTP
404, checked by `auth.sh`). Under hybrid auth a non-premium name is admitted
offline **directly** — no encryption offer, no drop, so the MCC AutoRelog
workaround added for the premium account is **removed** (`[ChatBot.AutoRelog]
Enabled = false`). The owner's real account `_alpha_01` is no longer used; it
will be locked to its Mojang uuid when the owner logs in officially.

The bot needs operator rights for `/gamemode`, `/give`, `/tp`, `/time`, etc.
Applied on fi2 (owner): `AETHER_OPERATORS` in `/etc/aether.env` is
`_alpha_01,Fire_z_g,Fire_z_g2,_aether_bot1@144.31.61.39` — bound to this
harness's source address `144.31.61.39`. Note the env var **overrides** the
`operators = [...]` list in `/opt/aether/aether-server.toml`, so the toml alone
would have been a no-op.

Coordinates in the coordinate-based scenarios were re-anchored to the bot's
world spawn (the old account had a personal spawn near `-8 70 8`; the bot
spawns at `(56.5, 63, 8.5)`), and `inventory-persist` now lists slot 36
directly instead of paging a large accumulated inventory.

## Live suite (10 scenarios × 5 versions)

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

50/50 pass.

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

- **The bot name is not premium** (404), so it is never offered encryption.
- **Invalid names** (`bad name`, a 20-char name, `ab`) are refused with
  `Invalid username.` and do not consume the rate-limit budget.
- **Rapid reconnects** hit the per-IP limit (`login_rate_per_min = 10`): 10
  admitted, then `Too many login attempts; try again in a minute.`
- **It recovers** after the 60 s window.
- **Offline play stays open** and the bot joins via MCC.

## Journal upgrade reminder

The `journal-upgrade` scenario still verifies `/inspect` on the old v3 region
(`-8 69 8`, ~13 h old entries written by the previous build) alongside a
freshly placed v4 block, and `/lookup` spanning both — all pass without a
decode error.

## Environment note

During earlier attempts the fi2 service was redeployed several times
(including a restart burst at 18:19–18:21 UTC and a final restart at
18:29:54 UTC); those runs failed with `Connection refused` and were discarded.
The run reported above was taken against a stable service.

## How to reproduce

```sh
export PATH="$HOME/.cargo/bin:/srv/dev/tools/bin:$PATH"
cd tools/live
bash all.sh          # the 10×5 client suite, then auth.sh -> last-run.md
bash auth.sh         # auth scenario alone (~2.5 min: waits out two 60 s windows)
```

No cargo build is involved in the live suite; it drives the prebuilt MCC client
at `/srv/dev/tools/mcc/MinecraftClient` and a stdlib Python login probe.
