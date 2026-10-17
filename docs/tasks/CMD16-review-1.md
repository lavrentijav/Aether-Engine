# CMD-16 review 1 — tags, teams, team messages

Reviewer: independent pass on `oc/CMD16` (bb13698) against `main` (8247549).
Scope: `/tag` (add/remove/list), `/team` (list/add/remove/empty/join/leave/modify +
teammsg), persistence, packets, CMD-06b selectors, 1.21.11 text/arguments, docs.

## Gates

All targeted gates pass on the review worktree (`CARGO_TARGET_DIR=/srv/dev/build/aether-target`,
`CARGO_INCREMENTAL=0`):

* `cargo fmt --all -- --check` — clean.
* `cargo clippy -p aether-server --all-targets -- -D warnings` — clean.
* `cargo test -p aether-server --lib` — 1505 passed, 0 failed.

The en_us strings were checked against the 1.21.11 language table and the command
surface against the 1.21.11 `commands.json` report; all emitted command strings
match vanilla verbatim (including the `team.notFound` wording). Vanilla sources
(`TagCommand`, `TeamCommand`, `TeamMsgCommand`, `ClientboundSetPlayerTeamPacket`,
`PlayerTeam`) were used for behaviour comparison.

## Findings

1. **HIGH — `collisionRule` is stored and sent but never affects collision.**
   `crates/aether-server/src/team.rs:245` defines the field and
   `crates/aether-server/src/commands/team_tag.rs:1032` sets it, but the only
   consumers are the team packet encoder (`protocol/scoreboard.rs:635,653,664`).
   No movement/collision path reads it (grep: `collision_rule` has no other
   reader), so `never` / `pushOwnTeam` / `pushOtherTeams` are cosmetic. The
   brief explicitly requires "`collisionRule` actually changes collision", and
   `docs/STATUS.md:137` also claims this works.

2. **HIGH — `deathMessageVisibility` has no effect and death messages ignore the
   team name formatting.**
   `crates/aether-server/src/team.rs:244` is only packed into the client packet
   (`:308`). `crates/aether-server/src/game/combat.rs:218` builds the death text
   from the raw player name and `:269` broadcasts it to every player
   unconditionally. Vanilla filters death-message recipients by
   `Team.getDeathMessageVisibility` and formats both names through
   `getFormattedName`, so `never` / `hideForOtherTeams` / `hideForOwnTeam` and
   team prefix/colour do not work. This breaks the "all effect rules" and
   "chat/… display paths read the team colour/prefix" requirement.

3. **MEDIUM — the `level=` selector compares total XP, not the XP level.**
   `crates/aether-server/src/commands/team_tag.rs:386` and
   `crates/aether-server/src/admin.rs:581` set `EntityRef.level = st.xp_total`.
   `EntityRef.level` is documented as experience level
   (`commands/selector.rs:98`) and `level=` filters on it (`:1172`), so
   `@a[level=0]` misses players with XP and `@a[level=30]` can match a level-1
   player with 30 total XP. This also regresses admin commands, which used to
   hard-code level 0; the correct source is `PlayerState::xp_level().0`.

4. **MEDIUM — non-player team members are invisible to selectors.**
   `/team join red @e[type=zombie]` records the entity's scoreboard name (the
   hyphenated UUID, `commands/team_tag.rs:831`), but `WorldSource::entities`
   unconditionally sets `team: None` (`commands/team_tag.rs:434`). Therefore
   `@e[team=red]` (and `/tag @e[team=red] …`) can never match a non-player team
   member, unlike vanilla. `team_of` / `team_name_of` are only consulted for
   players (`:398`, `admin.rs:594`).

5. **MEDIUM — the vanilla 1024-tag limit is not enforced.**
   Player tags are inserted without a cap at
   `crates/aether-server/src/commands/team_tag.rs:315` and non-player tags at
   `crates/aether-server/src/game/entities.rs:255`. Vanilla `Entity.addTag`
   refuses when the set already holds 1024 tags (the very reason for the
   "has too many tags" failure text), so this is both a behavioural divergence
   and an unbounded op-controlled allocation path.

6. **MEDIUM — the declared command tree uses the wrong argument parsers for
   `/team`.** The 1.21.11 report declares `minecraft:team` for every existing
   team argument, `minecraft:score_holder` (multiple) for join/leave members and
   `minecraft:color` for the colour value. The implementation declares:
   * `String` (`brigadier:string`) for `list/remove/empty/join/modify <team>` —
     `commands/team_tag.rs:136,145,148,151,186`;
   * `Entities` (`minecraft:entity`) for join/leave members —
     `:153,157`;
   * `String` for `modify <team> color <value>` — `:192`.
   Clients therefore receive a different Declare Commands tree: wrong parser
   ids, wrong suggestions and looser parsing (quoted team names with spaces,
   `dark-blue`, literal score-holder names, etc.). The server-side checks still
   work, but the argument surface is not vanilla-exact.

7. **MEDIUM/LOW — JSON components are flattened to legacy `§` text.**
   `component_text`/`render_component` (`commands/team_tag.rs:1178-1233`) render
   a component into a plain string; `TeamInfo` is then sent as one literal
   component (`protocol/scoreboard.rs:620-668`). Structural component features
   (hover/click/insertion, `translate` arguments — the key is rendered instead
   of its translation, nested styles beyond a single inherited style) are lost.
   The brief says JSON components are allowed, and vanilla preserves the
   component in display name/prefix/suffix. The "unchanged" comparisons for
   displayName/prefix/suffix also compare rendered text rather than components.

8. **LOW — colour parsing accepts values vanilla rejects.**
   `crates/aether-server/src/team.rs:58-63` strips every non-letter and
   lower-cases, so `dark-blue`, `darkblue` and even `dark_blue_extra` are
   accepted. `ColorArgument` only accepts the exact serialized names. The unit
   test pins this non-vanilla behaviour (`team.rs:415-418`).

9. **LOW — docs drift.**
   `/tag` usage still shows `<add,remove,list> <name>` although `list` takes no
   name: `crates/aether-server/src/commands/catalogue.rs:806`,
   `docs/COMMANDS.md:123`, `docs/COMMANDS.ru.md:123` (the real tree usage in
   `commands/team_tag.rs:124` is correct). `docs/STATUS.md:137` claims
   "collision rules" work, contradicted by finding 1 and by death-message
   visibility not being applied.

10. **LOW/MEDIUM — real teams can collide with the sidebar's pseudo-teams.**
    The sidebar creates teams named `sbl0`…`sbl14`
    (`crates/aether-server/src/protocol/scoreboard.rs:167`). `/team add sbl0`
    is now a legal real team. On protocols that use the Teams sidebar style,
    both writers use the same team packet kind, so `/team` updates and sidebar
    updates overwrite each other (user team options/lines flip); no reserved
    name space or guard exists. This is the concrete conflict with the existing
    sidebar/CMD-15 packet layer.

11. **LOW — team/argument validation order differs from vanilla.**
    `exec_team_join` resolves the member selector before checking that the team
    exists (`commands/team_tag.rs:699-703`), and the `modify` handlers parse the
    component before looking up the team (`:816-836`, `:1040-1053`, etc.).
    Vanilla resolves the `minecraft:team` argument first, so an unknown team
    with a bad selector/component reports `Unknown team '<name>'`, not the
    selector/component error.

12. **LOW — root nodes are executable although vanilla's are not.**
    `/tag`, `/team` and `/teammsg` are marked executable
    (`commands/team_tag.rs:116,224,240`), so a bare command returns a custom
    usage string; the 1.21.11 report leaves those literal roots
    non-executable. Harmless, but not vanilla-exact.

13. **LOW — team prefix/suffix above 64 characters are truncated on modern
    clients.** `protocol/scoreboard.rs:614-615` applies the sidebar's 64-char
    limit to real team components; vanilla `PlayerTeam` has no such limit, so a
    long prefix/suffix is silently cut in the packet. The 1.8 branch reuses
    `split_line`, which also folds any remainder of a long prefix into the
    suffix and drops the configured suffix.

## Verdict

**CHANGES.** The command grammar, en_us strings, permissions and packet shapes
are close to vanilla, and the persistence path is sound (teams under `TM`,
tags in save v3 with v1/v2 still read), but two required team effects are
missing (collision rule, death-message visibility), the `level=` selector is
wrong in both team/tag and admin selectors, non-player team members cannot be
selected, the tag cap is absent, and the declared `/team` argument tree is not
the 1.21.11 one. Findings 1–6 should be fixed before merge; 7–13 can be
follow-ups if explicitly documented as limitations.
