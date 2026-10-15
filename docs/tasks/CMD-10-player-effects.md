# CMD-10 — V-player: effects, items, attributes, damage

Status: 📋 task brief (CMD0) · size: L · core: yes
Needs: `oc/CMD-06`, `oc/CMD-05`, `oc/CMD-01`

## Split (CMD0 review)
This L group is split into:
| id | brief | contents | size |
|---|---|---|---|
| CMD-10a | `docs/tasks/CMD-10a-clear-effect-enchant.md` | `/clear` `/effect` `/enchant` | M |
| CMD-10b | `docs/tasks/CMD-10b-experience-attribute-damage.md` | `/experience` `/xp` `/attribute` `/damage` | M |

## Goal
Vanilla commands that change a player's/entity's state: inventory clearing, status effects,
enchantments, experience, attributes and direct damage. They reuse the existing game layer
(`game/effects.rs`, `game/combat.rs`, inventory/provenance), not new mechanics.

## Commands (vanilla 1.21.11)
| syntax | node | op | success feedback (en_us) |
|---|---|---|---|
| `/clear [<targets>] [<item>] [<maxCount>]` | `minecraft.command.clear` | 2 | `Removed <count> items from <player>` / `Found no items to remove` |
| `/effect give <targets> <effect> [<seconds>] [<amplifier>] [<hideParticles>]` | `minecraft.command.effect` | 2 | `Applied <effect> to <player>` |
| `/effect clear <targets> [<effect>]` | `minecraft.command.effect` | 2 | `Removed <effect> from <player>` / `Took <effect> from <player>` |
| `/enchant <targets> <enchantment> [<level>]` | `minecraft.command.enchant` | 2 | `Enchanted <item> with <enchantment> for <player>` |
| `/experience add <targets> <amount> [points|levels]` | `minecraft.command.experience` | 2 | `Gave <amount> experience to <player>` |
| `/experience set <targets> <amount> [points|levels]` | `minecraft.command.experience` | 2 | `Set <player>'s experience to <amount>` |
| `/experience query <targets> <points|levels>` | `minecraft.command.experience` | 2 | `<player> has <amount> experience` |
| `/attribute <target> <attribute> get [<scale>]` | `minecraft.command.attribute` | 2 | `<attribute> of <target> is <value>` |
| `/attribute <target> <attribute> base set <value>` / `base get [scale]` | `minecraft.command.attribute` | 2 | `Base <attribute> of <target> set to <value>` |
| `/attribute <target> <attribute> modifier add <id> <value> add\|multiply\|multiply_base` | `minecraft.command.attribute` | 2 | `Added modifier <id> to ...` |
| `/damage <target> <amount> [<type>] [at <loc>\|by <entity> [from <cause>]]` | `minecraft.command.damage` | 2 | `<target> was damaged` |

`/experience` alias `/xp`. `/clear` with an item performs the vanilla per-slot removal and reports
the true count; it must journal item destruction (provenance) like any other removal.

## Files
- new `crates/aether-server/src/game/commands_player.rs` (or split: `commands/effects.rs`).
- `game/effects.rs` (apply/remove), `game/provenance.rs` (clear), inventory model,
  `game/combat.rs` (damage), attribute storage (grep "attribute" — may not exist yet; if the
  player model has no attributes, add the minimum to satisfy `/attribute`).

## Edge cases to test explicitly
- `/clear` with an empty inventory → `Found no items to remove`; with `maxCount` removes at most
  that many; with an item filter removes only that item.
- `/effect give` with duration 0 uses the effect's default (or 1 tick?) and an 'infinite' flag;
  amplifier beyond the effect cap still applies; `hideParticles` toggles metadata for 1.21.11.
- `/enchant` on an empty hand / non-enchantable item → vanilla error; level range 1..255.
- `/experience add levels` at a very large amount clamps to the vanilla cap; `query` reports.
- `/attribute` unknown attribute/name errors; `modifier add` with a duplicate id replaces per
  vanilla rules.
- `/damage` with a damage type, with `by <entity>`, and with `at <location>`; a killing blow runs
  the normal death path once (no double death).
- every target list uses CMD-06; selectors may hit multiple entities.

## Done criteria
- Implementations + unit tests for the edge cases; combat/effect integration tests.
- fmt / clippy `-D warnings` / `cargo-locked test -p aether-server`.
