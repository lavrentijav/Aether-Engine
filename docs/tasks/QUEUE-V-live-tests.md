# V1 — live smoke tests on the test server (fi2) with MCC

Read `~/doc/mcc-testing.md` (how to run the patched MCC) and `docs/tasks/COMMON.md`.
Target: `fi2.walk.boykisser.online:25565`, account `_alpha_01` (operator, offline mode). It runs the
latest main. You have **no** shell on fi2: test only as a client. Do not edit engine code in this task.

## What to build
`tools/live/` in your worktree (branch `oc/V1`):
- `run.sh <scenario> [mc_version]` — runs MCC from a scratch dir under `/tmp/opencode/live/<scenario>-<ver>`
  with `MinecraftVersion` set, feeds the scenario's command script through stdin with sleeps, captures stdout
  (ANSI stripped) to a log, and evaluates **expectations** (grep patterns that must / must not appear).
  Exit code 0 = pass. A summary line per scenario: `PASS|FAIL <scenario> <version> <reason>`.
- `scenarios/*.txt` — one per scenario: lines `send <cmd>`, `wait <secs>`, `expect <regex>`,
  `reject <regex>` (e.g. `Unsupported|unsupported journal|unreadable|Disconnected|kicked`).
- `all.sh [versions...]` — runs every scenario on each version (default: 1.21.11 1.21.4 1.20.6 1.20.4 1.20.2)
  and prints a table; writes `tools/live/last-run.md` with the table and failing excerpts.

## Scenarios (start with these; MCC internal commands like `/dig`, `/move`, `/inventory`, `/useitem`,
`/look` exist only with terrain/inventory handling enabled in the ini — enable them)
1. **join** — joins, gets PlayerList with `_alpha_01`, no kick within 20 s.
2. **inspect** — `/inspect`, then hit/use a block in reach (`/look` down + `/dig` or use) → history text,
   **never** "unsupported journal format" / "incompatible". `/inspect` again turns it off.
3. **place-break-rollback** — give self stone (`/give`), place, break, `/rollback` (or the server's rollback
   command — check `/help`), inspect shows the events.
4. **inventory-persist** — `/give` items, `/quit`, rejoin, `/inventory` shows them (no "unreadable").
5. **eat-effects** — give a golden apple, eat it, effect message / no disconnect.
6. **container** — place a chest, open it (`/useblock`), put an item, reopen, item still there.
7. **potion** — give a potion, drink; give a splash potion, throw; no disconnect.
8. **commands** — `/time set day`, `/gamemode creative`, `/tp`, `/spawn`, `/help` respond.
9. **multi-version join** — scenario 1 on every version in the default list.

## Rules
- Never spam: max 1 scenario at a time, ≥5 s between runs (it is a shared test server).
- If a scenario fails, record the exact output excerpt; do not "fix" the expectation to make it pass.
- Commit the harness and `last-run.md` (V1a, V1b, …). Report the table and every failure with excerpt.
