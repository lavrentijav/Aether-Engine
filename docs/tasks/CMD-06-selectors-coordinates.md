# CMD-06 — Selector and coordinate parser (umbrella)

Status: 📋 umbrella brief (CMD0). Split into **CMD-06a** and **CMD-06b**. tier: core (C).
Needs: `oc/CMD-03`.

## Goal
One shared parser for entity selectors (`@a @p @r @s @e @n`), coordinates (`~` relative, `^` local,
absolute), rotations, and the vanilla argument types that carry them. Every command brief from
CMD-08 on uses this; do not re-implement selectors per command.

## Split
| id | brief | contents | size |
|---|---|---|---|
| CMD-06a | `docs/tasks/CMD-06a-coordinates-args.md` | argument types + coordinates `~ ^`, rotations, block/item/NBT arguments | M |
| CMD-06b | `docs/tasks/CMD-06b-selectors.md` | `@a/@p/@r/@s/@e/@n`, `[...]` filters, sort/limit, selection semantics | M |

Most commands depend on **CMD-06b** (which depends on CMD-06a), so they get both parsers.

## Done criteria
- Per the sub-briefs; fmt / clippy `-D warnings` / `cargo-locked test`.
