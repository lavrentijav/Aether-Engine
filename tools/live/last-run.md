# V1 live run

- Date: 2026-10-15 17:38 UTC
- Target: fi2.walk.boykisser.online:25565, account `_alpha_01` (operator, offline mode)
- Server: fi2 main `4092d2c` with AU1 hybrid auth (`[auth] mode = "hybrid"`);
  deployed 2026-10-15 16:46:38 UTC, binary sha256 `d5b1bbb30797a2ef…`. The
  service restarted twice mid-run (17:18:19 and 17:20:47 UTC, same binary) —
  see the `potion 1.20.6` note.
- Versions: 1.21.11 1.21.4 1.20.6 1.20.4 1.20.2
- Harness: `tools/live/run.sh` + `all.sh` (MCC 26.2) and `tools/live/auth.sh`
  (raw login probe); scratch in `/tmp/opencode/live`.

## Live suite (10 scenarios × 5 versions)

| scenario | 1.21.11 | 1.21.4 | 1.20.6 | 1.20.4 | 1.20.2 |
| --- | --- | --- | --- | --- | --- |
| join | PASS | PASS | PASS | PASS | PASS |
| inspect | PASS | PASS | PASS | PASS | PASS |
| place-break-rollback | PASS | PASS | PASS | PASS | PASS |
| inventory-persist | PASS | PASS | PASS | PASS | PASS |
| eat-effects | PASS | PASS | PASS | PASS | PASS |
| container | PASS | PASS | PASS | PASS | PASS |
| potion | PASS | PASS | **FAIL→PASS** | PASS | PASS |
| commands | PASS | PASS | PASS | PASS | PASS |
| journal-upgrade | PASS | PASS | PASS | PASS | PASS |
| multi-version-join | PASS | PASS | PASS | PASS | PASS |

49/50 on the first pass; 50/50 after re-running the one flake.

### `potion 1.20.6` — server restart, not a code failure

The one failure was caused by a fi2 redeploy during the run, not by the server
code: MCC logged `Connection has been lost` then `[ERROR] Connection refused
[::ffff:2.26.115.4]:25565` (the service was down at 17:20:47 UTC), and the
scenario's later commands were read while disconnected:

```
[MCC] Requested Potion x1 in slot #36
[MCC] Requested SplashPotion x1 in slot #37
[MCC] Connection has been lost.
...
[ERROR] Connection refused [::ffff:2.26.115.4]:25565
[ERROR] Failed to connect to this IP.
...
Unknown command 'changeslot'. Use 'help' for command list.
```

Re-run after the restart: `PASS potion 1.20.6`.

## Auth (AU1 hybrid) — `bash auth.sh`

All 7 checks pass:

```
PASS auth invalid-name-space refused: {"text":"Invalid username."}
PASS auth invalid-name-long refused: {"text":"Invalid username."}
PASS auth invalid-name-short refused: {"text":"Invalid username."}
PASS auth login-rate-limit admitted=10 then refused: refused: {"text":"Too many login attempts; try again in a minute."}
PASS auth login-rate-recovery admitted after 62s
PASS auth offline-open admitted	id=0x03
PASS auth operator-offline-admitted PASS join 1.21.11
```

- **`_alpha_01` (offline) is still admitted.** The join scenario passes on all
  five versions.
- **Invalid names are refused** with a clear message, before any Mojang work:
  `bad name`, a 20-char name and `ab` all get `Invalid username.` (vanilla's
  3–16 × `[A-Za-z0-9_]` rule). These attempts do not consume the rate-limit
  budget, so they run free.
- **Rapid reconnects hit the per-IP limit.** A clean-window burst admits 10
  then the 11th is refused `Too many login attempts; try again in a minute.` —
  the `login_rate_per_min = 10` default.
- **It recovers.** 62 s later (past the limiter's 60 s window) the same address
  is admitted again.
- **Offline play stays open.** A fresh, non-premium name joins offline.

### Note: `_alpha_01` is a *premium* Mojang name

`https://api.mojang.com/users/profiles/minecraft/_alpha_01` → HTTP 200 (uuid
`3aaab7b3e24847f3aa959e395cdb39ac`). Under hybrid policy an unlocked premium
name is *offered* encryption; an offline client (MCC with password `-`) cannot
answer it, so it drops the connection. The server then records the
same-address "dropped during online auth" fallback and admits that name
offline on the next attempt within 300 s:

```
[auth] _alpha_01 dropped during online auth; will admit offline from this address for the next 300s
[auth] _alpha_01 dropped during a recent online auth from this address; offline without an encryption offer
[+] _alpha_01 joined (eid 2, c1bb91ad-… via 1.21.11, offline auth, from 144.31.61.39)
```

So a *single* offline attempt as `_alpha_01` fails (`[ERROR]
InvalidOperationException: Queue empty.` in MCC); it takes two attempts. To let
the gameplay suite work unchanged, `mcc-template.ini` now enables
`[ChatBot.AutoRelog]` (1 s delay, unlimited retries, only on network
interruptions — not on server kicks). MCC then reconnects and is admitted
offline via the fallback. This is a property of choosing a premium operator
name on a hybrid server, not a harness bug.

Security note (for AU1, not this task): because the fallback admits an unlocked
premium name offline after a drop, an offline client can claim `_alpha_01` —
the operator — by dropping once and reconnecting. Binding operators to an
address (`AETHER_OPERATORS=_alpha_01@<ip>`) or locking the name would close it;
the server itself already warns about the bare-name binding at startup.

## How to reproduce

```sh
export PATH="$HOME/.cargo/bin:/srv/dev/tools/bin:$PATH"
cd tools/live
bash all.sh          # the 10×5 client suite, then auth.sh -> last-run.md
bash auth.sh         # the auth scenario on its own (~2.5 min: waits out two 60 s windows)
```

No cargo build is involved in the live suite; it drives the prebuilt MCC client
at `/srv/dev/tools/mcc/MinecraftClient` and a stdlib Python login probe.
