# CMD-33b — Plugin: permissions meta, tracks, editor

Status: 📋 task brief (CMD0) · size: M · tier: native plugin (N) · plugin: `plugins/permissions`
Needs: `oc/CMD-00N`, `oc/P1`, `oc/CMD-02`, `oc/CMD-33a`
Parent/umbrella: `docs/tasks/CMD-33-permissions-ui.md`

## Commands (LuckPerms reference)
| command | syntax | node |
|---|---|---|
| temp nodes | `/lp user\|group <t> permission settemp <node> <true\|false> <duration> [world=<world>]` / `unsettemp <node>` (expiry restores the previous value) | `luckperms.user.permission.settemp` / `.unsettemp` |
| meta | `/lp user\|group <t> meta info\|set\|add\|remove\|settemp <prefix\|suffix\|weight> …` | `luckperms.meta.*` |
| tracks | `/lp track <track> info\|create\|delete\|append\|insert\|remove` | `luckperms.track.*` |
| context | `/lp user <u> permission set <node> true world=<world>` | `luckperms.context.*` |
| editor | `/lp editor` → one-time web URL | `luckperms.editor` |
| sync/verbose | `/lp sync`, `/lp verbose on\|off` | `luckperms.sync` / `.verbose` |

## Files
- `plugins/permissions/`: manifest, `src/`, README.
- prefix/suffix meta read by the chat plugin (CMD-40); the editor uses the net capability (PA3)
  when granted and is disabled otherwise.

## Edge cases to test explicitly
- meta prefix/suffix/weight reflect in chat; a weight that is not an integer errors.
- a temporary node expires and the previous value (explicit, inherited or default) returns; the
  remaining duration survives a restart, and `unsettemp` removes only the temporary entry.
- context-specific node then a global node; the context one wins in that world.
- the editor URL is one-time and expires; disabled without PA3.
- concurrent editor and command-line edits cannot lose data (ledger/transaction).

## Done criteria
- Plugin + tests; fmt / clippy `-D warnings` / `cargo-locked test`.
