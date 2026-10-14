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
// **Block items are a placeholder here.** A block item a version lacks should
// follow the *block* substitution rules (`blocks.json`, task J2), which the
// block compat layer owns; until that is merged the generator maps such an
// item to a generic stand-in (`barrier`) and records it, so the item layer is
// independent of the block tables. TODO(J1/J2): replace `placeholderItem`
// with a lookup through the merged block rules and regenerate.
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

const outDir =
  process.argv[2] || 'crates/aether-server/src/protocol/compat/item_gen'

const BASE = '1.21.11'
const base = d(BASE)

// codec module name -> minecraft-data version.
const VERSIONS = [
  ['v1_8', '1.8'],
  ['v1_17', '1.17'],
  ['v1_17_1', '1.17.1'],
  ['v1_18', '1.18'],
  ['v1_18_2', '1.18.2'],
  ['v1_19', '1.19'],
  ['v1_19_2', '1.19.2'],
  ['v1_19_3', '1.19.3'],
  ['v1_19_4', '1.19.4'],
  ['v1_20_1', '1.20.1'],
  ['v1_20_5', '1.20.5'],
  ['v1_21_1', '1.21.1'],
  ['v1_21_3', '1.21.3'],
  ['v1_21_4', '1.21.4'],
  ['v1_21_5', '1.21.5'],
  ['v1_21_6', '1.21.6'],
  ['v1_21_8', '1.21.8'],
  ['v1_21_9', '1.21.9'],
]

const strip = n => n.replace(/^minecraft:/, '')
const prefix = n => (n.includes(':') ? n : 'minecraft:' + n)

// --- substitution rules -----------------------------------------------------
function loadRules(file) {
  const raw = JSON.parse(fs.readFileSync(path.join(__dirname, file), 'utf8'))
  const list = Array.isArray(raw)
    ? raw.flatMap(x => (x && x.rules ? x.rules : [x]))
    : raw.rules
  return (list || [])
    .filter(r => r && r.from && r.to)
    .map(r => ({ from: strip(r.from), to: strip(r.to) }))
}

function glob(pattern, name) {
  const star = pattern.indexOf('*')
  if (star < 0) return name === pattern ? '' : null
  const pre = pattern.slice(0, star)
  const post = pattern.slice(star + 1)
  if (name.length < pre.length + post.length) return null
  if (!name.startsWith(pre) || !name.endsWith(post)) return null
  return name.slice(pre.length, name.length - post.length)
}

// Resolve a name the version lacks to one it has, applying rules repeatedly.
function resolver(names, rules) {
  return function resolve(name) {
    let cur = name
    for (let i = 0; i < 16; i++) {
      let next = null
      for (const r of rules) {
        const cap = glob(r.from, cur)
        if (cap !== null) {
          next = r.to.replace('*', cap)
          break
        }
      }
      if (next === null) return null
      if (names.has(next)) return next
      if (next === cur) return null
      cur = next
    }
    return null
  }
}

// --- items ------------------------------------------------------------------
const BASE_ITEMS = base.itemsArray.slice().sort((a, b) => a.id - b.id)
const ENGINE_ITEMS = BASE_ITEMS.length

// The placeholder for a block item the version lacks, until the block rules
// are merged in. A real item the version has, so the client still sees
// something; see the header.
function placeholderItem(names) {
  for (const candidate of ['barrier', 'stone', 'oak_planks', 'dirt']) {
    if (names.has(candidate)) return candidate
  }
  return null
}

function buildItems(mcVersion, itemRules) {
  const t = d(mcVersion)
  const byName = {}
  for (const it of t.itemsArray) byName[it.name] = it
  const names = new Set(Object.keys(byName))
  // Which names are block items, so a missing one takes the placeholder
  // rather than a pure-item rule that will not match.
  const blockNames = new Set((t.blocksArray || []).map(b => b.name))
  const resolve = resolver(names, itemRules)

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
      let sub = resolve(name)
      // TODO(J1/J2): a block item should follow the block rules; until they
      // are merged, a generic placeholder stands in.
      if (!sub && blockNames.has(name)) sub = placeholderItem(names)
      if (sub) {
        id = byName[sub].id
        subst = true
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

function writeEnglish() {
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

function itemLines(mcVersion, itemRules) {
  const { map, byId, fallbacks } = buildItems(mcVersion, itemRules)
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

fs.mkdirSync(outDir, { recursive: true })
const itemRules = loadRules('items.json')
writeEnglish()
console.log(
  `engine items ${ENGINE_ITEMS}, item rules ${itemRules.length}, placeholder block-item stand-in`,
)
let total = 0
for (const [mod, mcVersion] of VERSIONS) {
  const it = itemLines(mcVersion, itemRules)
  const out = [
    `//! Compat **item** tables for ${mcVersion} (module \`${mod}\`).`,
    '//!',
    '//! Generated by `tools/gen/compat/gen_items.js` from PrismarineJS',
    '//! `minecraft-data` and the item rules beside it (`items.json`). Do not',
    '//! edit by hand. Block items are a placeholder until the block rules are',
    '//! merged; see the generator header.',
    '',
    ...it.lines,
  ]
  fs.writeFileSync(path.join(outDir, mod + '.rs'), out.join('\n') + '\n')
  total += it.fallbacks
  console.log(`${mod.padEnd(9)} ${mcVersion.padEnd(7)} item-fb ${it.fallbacks}`)
}
console.log(`total item fallbacks ${total}`)
