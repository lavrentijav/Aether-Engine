# Rules for every opencode queue (read before your queue file)

- **Shared build dir, mandatory:** `export CARGO_TARGET_DIR=/srv/dev/build/aether-target` in every
  shell before any cargo command. All agents share compiled dependencies there, and cargo's own
  lock on that dir serialises builds — waiting on "Blocking waiting for file lock on build
  directory" is expected, do not work around it. Never use a private target dir. `-j4`.
- One worktree per queue: `git -C /srv/dev/projects/Aether-Engine worktree add
  /srv/dev/projects/Aether-Engine-wt/<Q> -b oc/<Q>`; never edit the shared checkout.
- After each task: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -j4 -- -D warnings`,
  tests of the crates you touched; one commit per task, message starts with the task id.
- No push, merge, sudo, or edits outside your worktree. Blocked → write why in
  `docs/tasks/<Q>-notes.md`, commit, continue. Report at the end: branch, hash per task, checks.
- Behaviour must match vanilla 1.21.11 (data pack / server behaviour), not a look-alike. Check
  `docs/KNOWN_ISSUES.md` first; update it (and `docs/STATUS.md`) when you close an item.
- **Core or plugin:** an optional, non-performance-critical feature (economy, shops, rewards, kits,
  admin UIs, web integrations, custom modes) is written as a plugin on the plugin API
  (`docs/design/plugins.md`), not in the core; if the API lacks something, extend the API. See
  "Core or plugin?" in `CONTRIBUTING.md`.
- **Tests through `cargo-locked`:** worktrees share `CARGO_TARGET_DIR` and their artifacts have the
  same file names there, so a plain `cargo test` can run a binary another agent just rebuilt. Run
  `cargo-locked test ...` (and `cargo-locked run/bench`): it holds one lock for build + run. Plain
  `cargo check`/`clippy`/`fmt` are fine. A report "tests pass" is only valid from `cargo-locked`.
- **Disk:** also `export CARGO_INCREMENTAL=0` with the shared target dir: incremental caches of a
  dozen worktrees filled the disk (26 GB). Builds are slightly slower, the disk survives.
