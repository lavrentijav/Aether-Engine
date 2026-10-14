# Queue B — vanilla worldgen features still missing (do in order)

Repo `/srv/dev/projects/Aether-Engine`, crate `crates/aether-worldgen`. Read
`src/vanilla/mod.rs` (header lists what is missing), `src/vanilla/feature/*.rs` (how placed
features are ported), and `docs/KNOWN_ISSUES.md` item 8. Port each feature **from the game's data
pack JSON and the 1.21.11 server code behaviour**, the same way existing features are done, using the
same RNG/seeding helpers — no hand-tuned look-alikes.

One worktree for the queue:
`git -C /srv/dev/projects/Aether-Engine worktree add /srv/dev/projects/Aether-Engine-wt/QB -b oc/QB`.
Build only there, `-j4`. After EACH feature: fmt, `cargo clippy --workspace --all-targets -j4 -- -D warnings`,
`cargo test -p aether-worldgen -j4` pass; add a unit test that generates a known seed/chunk and
asserts the feature's blocks appear (and a determinism test: same seed twice → same blocks); one
commit per feature. If a parity example (`examples/vanilla_world_compare.rs`) can be run without the
game jar, report its numbers before/after. No push/merge/sudo, no edits outside the worktree.
If blocked, write why in `docs/tasks/QUEUE-B-notes.md`, commit, continue.

1. B1 `desert_well`
2. B2 `monster_room` (dungeons; chest loot may be empty for now — note it)
3. B3 `geode` (amethyst)
4. B4 `fossil` (+ `fossil_diamonds` / coal variants as in the pack)
5. B5 pointed dripstone: `dripstone_cluster`, `large_dripstone`, `pointed_dripstone`
6. B6 multiface spreading for `glow_lichen` (currently placed without spreading)
Update `src/vanilla/mod.rs` header and `docs/KNOWN_ISSUES.md` item 8 as features land.
