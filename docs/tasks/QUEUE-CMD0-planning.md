# CMD0 — turn the command catalogue into tasks + stubs

Read `docs/tasks/COMMON.md`, `docs/design/commands.md` (the catalogue with core/plugin decisions — follow
them; if you think one is wrong, say so in your report, do not silently change it), `docs/design/plugins.md`
and `CONTRIBUTING.md` "Core or plugin?". Look at how commands work today: `crates/aether-server/src/commands/`,
`game/commands.rs`, `economy/commands.rs`.

## Deliverables (branch `oc/CMD0`)

1. **Task briefs** `docs/tasks/CMD-<id>-<slug>.md`, one per sensible unit of work (about one agent session
   each; split big groups), standalone, in English, each with: goal, exact commands and their vanilla /
   plugin-reference behaviour (syntax, permission node, op level, feedback text), files to touch,
   **edge cases to test explicitly**, dependencies (`needs:` other CMD ids or branches like `oc/P1`), done
   criteria. Cover the whole catalogue: core infra (Brigadier tree + Declare Commands per codec, permissions
   core, selectors/coordinates parser), vanilla groups V-admin/V-player/V-world/V-data (split further),
   server ops, history extensions, the core APIs for plugins (bulk edit, region index + protection check,
   teleport/safe spot, player API), and every plugin in section 4 plus regions/claims/worldedit/permissions UI.
2. **Stubs** (compile, tests green):
   - core: a command registry entry for every core command in the catalogue that does not exist yet, with
     its permission node and op level, replying `not implemented yet (CMD-<id>)`; `/help` lists them.
     Existing commands untouched.
   - plugins: `plugins/<name>/` skeletons (manifest `plugin.toml`, README with its command list and
     the CMD task id) — no code that depends on the plugin host API until `oc/P1` is merged; if P1 is not
     in main, only manifests + READMEs.
3. **Queue manifest** `docs/tasks/CMD-queue.json`: an array of
   `{"id": "CMD-…", "title": "…", "brief": "docs/tasks/CMD-….md", "needs": ["oc/CMD-…" or "oc/P1"], "size": "S|M|L", "core": true|false}`
   in a dependency-correct order. Do not start any of the tasks yourself.

Commit per deliverable (CMD0a briefs, CMD0b core stubs, CMD0c plugin skeletons, CMD0d queue manifest).
Env CARGO_INCREMENTAL=0; tests via cargo-locked test. Report: number of tasks, the dependency order, and
any catalogue decision you disagree with.
