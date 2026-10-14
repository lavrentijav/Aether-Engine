# Queue G — bring every README / *.md up to date (docs only)

Repo `/srv/dev/projects/Aether-Engine`. Worktree: `git -C /srv/dev/projects/Aether-Engine worktree add
/srv/dev/projects/Aether-Engine-wt/QG -b oc/QG`. No code changes, no cargo builds needed (if you
must check something compiles, follow `docs/tasks/COMMON.md`).

Scope: `README.md`, `README.ru.md`, `CONTRIBUTING*.md`, `CLAUDE.md`, `docs/*.md` (STATUS, ROADMAP,
KNOWN_ISSUES, DESIGN_NOTES, both languages), `deploy/README.md`, crate-level READMEs if any.
Out of scope: `docs/tasks/**`, `docs/storage-v2/**`.

Method — every claim checked against the code, not rewritten from memory:
1. For each statement about what exists (versions/protocols supported, crates, commands, config
   keys, features, toolchain, CI steps, limits) find it in the code (`grep`, read the module) and fix
   it if wrong. Recent changes to reflect: pinned toolchain 1.99.0 + `deny.toml` + cargo-deny in CI;
   `max_players` enforced and `max_connections` / `max_connections_per_ip`; unreadable saves refuse
   login instead of becoming empty; journal sequence reservation; item ids `ItemTypeId(u16)` and
   64-bit item instance ids; storage v2 direction (link `docs/storage-v2/README.md` from ROADMAP and
   DESIGN_NOTES §10 instead of duplicating it).
2. English and Russian versions must say the same thing; fix drift between them.
3. Remove obsolete statements (don't keep "~~fixed~~" history in KNOWN_ISSUES unless it explains a
   current limitation — git history keeps the rest). Keep the existing tone and structure.
4. Commit per file group (README pair, CONTRIBUTING pair, each docs/ pair). Message starts `QG:`.
Other agents may edit KNOWN_ISSUES/STATUS on their branches; that is fine, keep your edits focused.
Report: branch, hashes, a list of the most important corrections (what was wrong → what is true).
No push/merge/sudo, no edits outside the worktree.
