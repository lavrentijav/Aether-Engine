// Generates the per-release **item** tables of the version-compat layer
// (queue J, task J3).
//
//   node tools/gen/compat/gen_items.js <out-dir>
//
// Reads PrismarineJS `minecraft-data` (the same source `registry/items.rs`
// was pinned to) and the item substitution rules in `items.json` beside this
// file. The engine's item ids are vanilla 1.21.11's, so every table is
// indexed by them.
//
// For each supported older release it writes one Rust module:
//
//   * `ITEM_MAP` — one entry per engine item id: the version's wire id in the
//     low 16 bits, `-1` when the version has no stand-in, and bit 16 set when
//     the id is a *substitute* (a different item stands in, so the client is
//     shown the real name).
//   * `ITEM_BY_ID` — the version's own item names by wire id.
//   * `ITEM_FALLBACKS` — engine items the version lacks and no rule covers.
//
// **Block items follow the block rules.** A block *item* a version lacks is
// resolved through the same `blocks.json` rules (and the same resolver) the
// block half of the layer uses (task J10), so the block a player places and
// the item they hold downgrade to the same stand-in. The star of the
// resolution is `../gen.js`, which owns the rules, the globbing and the
// chaining; it is required as a module here (it only generates when run
// directly). A block item no rule covers takes the version's stone item, the
// same last resort the block table uses, so it never lands in
// `ITEM_FALLBACKS` — that list is for items that are not blocks.
'use strict'
const fs = require('fs')
const path = require('path')

// `minecraft-data` lives beside the probe tooling or in a checkout-local
// `node_modules`; try the same places `gen.js` does, then a couple of common
// ones, so the script runs from the repo root.
const requirePaths = [
  process.cwd(),
  path.join(__dirname, '..', '..', 'probe'),
  '/tmp/opencode/repro',
  path.join(process.env.HOME || '', 'node_modules'),
]
const d = require(require.resolve('minecraft-data', { paths: requirePaths }))

const BASE = '1.21.11'
const base = d(BASE)

// codec module name -> (minecraft-data version, flattened?). Only 1.8 is
// pre-flattening, so only it sees the legacy-only block rules (T4); a
// flattened version's item stand-in must match its block table's, which drops
// them.
const VERSIONS = [
  ['v1_8', '1.8', false],
  ['v1_17', '1.17', true],
  ['v1_17_1', '1.17.1', true],
  ['v1_18', '1.18', true],
  ['v1_18_2', '1.18.2', true],
  ['v1_19', '1.19', true],
  ['v1_19_2', '1.19.2', true],
  ['v1_19_3', '1.19.3', true],
  ['v1_19_4', '1.19.4', true],
  ['v1_20_1', '1.20.1', true],
  ['v1_20_2', '1.20.2', true],
  ['v1_20_4', '1.20.4', true],
  ['v1_20_5', '1.20.5', true],
  ['v1_21_1', '1.21.1', true],
  ['v1_21_3', '1.21.3', true],
  ['v1_21_4', '1.21.4', true],
  ['v1_21_5', '1.21.5', true],
  ['v1_21_6', '1.21.6', true],
  ['v1_21_8', '1.21.8', true],
  ['v1_21_9', '1.21.9', true],
]

// The block-rule resolver, rule loader and name helpers all live in `gen.js`
// (task J2) so the block and item halves cannot drift apart.
const blockGen = require('./gen.js')
const { strip, prefix, resolver, loadRules } = blockGen

// --- items ------------------------------------------------------------------
const BASE_ITEMS = base.itemsArray.slice().sort((a, b) => a.id - b.id)
const ENGINE_ITEMS = BASE_ITEMS.length

// The engine's own block names. An engine item whose name is one of these is a
// *block item*, so it follows the block rules rather than the item rules.
const BASE_BLOCKS = new Set(base.blocksArray.map(b => b.name))

// A few 1.8 blocks have no item of the same name — the item that places them
// goes by another name (the sign and banner blocks against `sign`/`banner`,
// lit/state variants against their base). Map the legacy block name to its
// item so a block item follows its block (F7) instead of falling to stone.
// Anything not here and without a same-named item (fire, water, portal) has no
// item at all and stays stone.
const LEGACY_BLOCK_ITEMS = {
  standing_sign: 'sign',
  wall_sign: 'sign',
  standing_banner: 'banner',
  wall_banner: 'banner',
  lit_furnace: 'furnace',
  unlit_redstone_torch: 'redstone_torch',
  lit_redstone_ore: 'redstone_ore',
  unpowered_repeater: 'repeater',
  powered_repeater: 'repeater',
  unpowered_comparator: 'comparator',
  powered_comparator: 'comparator',
  lit_redstone_lamp: 'redstone_lamp',
  daylight_detector_inverted: 'daylight_detector',
  piston_head: 'piston',
  piston_extension: 'piston',
  double_stone_slab: 'stone_slab',
  double_wooden_slab: 'wooden_slab',
  double_stone_slab2: 'stone_slab',
  carrots: 'carrot',
  potatoes: 'potato',
  tripwire: 'string',
  redstone_wire: 'redstone',
  // 1.8 has no `fire` item; the way to make fire there is flint and steel, so a
  // campfire item becomes that rather than stone. The block stays `fire` (a
  // fire source) — the icon differs but the pair is thematically consistent.
  fire: 'flint_and_steel',
}

function buildItems(mcVersion, itemRules, blockRules) {
  const t = d(mcVersion)
  const byName = {}
  for (const it of t.itemsArray) byName[it.name] = it
  const names = new Set(Object.keys(byName))
  // Block *names* the version has, for the block resolver. Its output is a
  // block name; the item of that name is the stand-in (they always share a
  // name), so it is looked up in `byName`.
  const blockNames = new Set((t.blocksArray || []).map(b => b.name))
  // The shared resolver returns `{ name, rule, cap }`; the item layer only
  // needs the name, but `gen.js` uses the rule's metadata for the 1.8 blocks.
  const resolveItem = resolver(names, itemRules)
  const resolveBlock = resolver(blockNames, blockRules)
  const stone = names.has('stone') ? 'stone' : null

  const maxId = t.itemsArray.reduce((a, it) => Math.max(a, it.id), 0)
  const map = new Array(ENGINE_ITEMS).fill(-1)
  const fallbacks = []
  for (let e = 0; e < ENGINE_ITEMS; e++) {
    const name = strip(BASE_ITEMS[e].name)
    let id
    let subst = false
    if (names.has(name)) {
      id = byName[name].id
    } else {
      // A block item follows its *block's* substitution — the block rules are
      // tried first, never the item rules, so the held item and the placed
      // block downgrade together (F5). The block rules chain, and a block no
      // rule covers becomes stone, exactly as the block table does, so a block
      // item never ends in `ITEM_FALLBACKS`. If the resolved block has no item
      // of that name in this version, stone is the last resort.
      let sub
      if (BASE_BLOCKS.has(name)) {
        const block = resolveBlock(name)?.name ?? null
        // The resolved block's item: the same name, or the legacy alias when
        // the block has no same-named item (F7).
        const alias = block ? LEGACY_BLOCK_ITEMS[block] : null
        const itemName =
          block && names.has(block) ? block : alias && names.has(alias) ? alias : null
        sub = itemName ?? stone
      } else {
        sub = resolveItem(name)?.name ?? null
      }
      if (sub && byName[sub]) {
        id = byName[sub].id
        subst = sub !== name
      } else {
        fallbacks.push(prefix(name))
        continue
      }
    }
    map[e] = (id & 0xffff) | (subst ? 0x1_0000 : 0)
  }

  const byId = new Array(maxId + 1).fill('')
  for (const it of t.itemsArray) byId[it.id] = prefix(it.name)
  return { map, byId, fallbacks: fallbacks.sort() }
}

function writeEnglish(outDir) {
  const out = []
  const w = s => out.push(s)
  w("//! English display names of the engine's items, indexed by engine item id.")
  w('//!')
  w('//! Generated by `tools/gen/compat/gen_items.js` from PrismarineJS')
  w(`//! \`minecraft-data\` (pc/${BASE}, MIT). Do not edit by hand.`)
  w('')
  w('/// The name a *substituted* item is shown under, so the client reads what')
  w('/// the server really holds even though the icon is a stand-in.')
  w('pub static ENGLISH: &[&str] = &[')
  let row = []
  BASE_ITEMS.forEach(it => {
    row.push(JSON.stringify(it.displayName || it.name))
    if (row.length === 6) {
      w('    ' + row.join(', ') + ',')
      row = []
    }
  })
  if (row.length) w('    ' + row.join(', ') + ',')
  w('];')
  w('')
  fs.writeFileSync(path.join(outDir, 'english.rs'), out.join('\n') + '\n')
}

function itemLines(mcVersion, itemRules, blockRules) {
  const { map, byId, fallbacks } = buildItems(mcVersion, itemRules, blockRules)
  const out = []
  const w = s => out.push(s)
  w("/// One entry per engine item id, in id order: the version's wire id in the")
  w('/// low 16 bits, `-1` when the version has no stand-in, and bit 16 set when')
  w('/// the id is a *substitute* (a different item stands in for the real one,')
  w('/// so the client is shown the real name).')
  w('pub static ITEM_MAP: &[i32] = &[')
  let row = []
  map.forEach(v => {
    row.push(v)
    if (row.length === 20) {
      w('    ' + row.join(', ') + ',')
      row = []
    }
  })
  if (row.length) w('    ' + row.join(', ') + ',')
  w('];')
  w('')
  w("/// The version's own item names by wire id (`\"\"` for an unused id).")
  w(
    'pub static ITEM_BY_ID: &[&str] = &[' +
      byId.map(n => JSON.stringify(n)).join(', ') +
      '];',
  )
  w('')
  w('/// Engine items the version lacks and no rule covers; the caller shows a')
  w('/// generic stand-in instead. See docs/KNOWN_ISSUES.md.')
  w(
    'pub static ITEM_FALLBACKS: &[&str] = &[' +
      fallbacks.map(n => JSON.stringify(n)).join(', ') +
      '];',
  )
  w('')
  return { lines: out, fallbacks: fallbacks.length }
}

function main() {
  const outDir =
    process.argv[2] || 'crates/aether-server/src/protocol/compat/item_gen'
  fs.mkdirSync(outDir, { recursive: true })
  const itemRules = loadRules('items.json')
  const blockRules = loadRules('blocks.json')
  writeEnglish(outDir)
  console.log(
    `engine items ${ENGINE_ITEMS}, item rules ${itemRules.length}, ` +
      `block rules ${blockRules.length} for block items`,
  )
  let total = 0
  for (const [mod, mcVersion, flat] of VERSIONS) {
    // A flattened version drops the legacy-only block rules (T4), so a block
    // item's stand-in is exactly the one its block table uses.
    const br = flat ? blockRules.filter(r => !r.legacy) : blockRules
    const ir = flat ? itemRules.filter(r => !r.legacy) : itemRules
    const it = itemLines(mcVersion, ir, br)
    const out = [
      `//! Compat **item** tables for ${mcVersion} (module \`${mod}\`).`,
      '//!',
      '//! Generated by `tools/gen/compat/gen_items.js` from PrismarineJS',
      '//! `minecraft-data`, the item rules beside it (`items.json`) and the',
      '//! block rules in `blocks.json` (a block item follows its block). Do',
      '//! not edit by hand.',
      '',
      ...it.lines,
    ]
    fs.writeFileSync(path.join(outDir, mod + '.rs'), out.join('\n') + '\n')
    total += it.fallbacks
    console.log(`${mod.padEnd(9)} ${mcVersion.padEnd(7)} item-fb ${it.fallbacks}`)
  }
  console.log(`total item fallbacks ${total}`)
}

if (require.main === module) main()
