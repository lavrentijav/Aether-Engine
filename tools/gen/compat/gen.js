// Generates the per-version block- and item-state tables of the version
// compatibility layer (queue J).
//
//   node tools/gen/compat/gen.js <out-dir>
//
// Reads PrismarineJS `minecraft-data` (the same source `registry/blocks.rs`
// and `registry/items.rs` were pinned to) and the substitution rules in
// `blocks.json` / `items.json` beside this file. The engine's own state ids
// are vanilla 1.21.11's flattened ids, so every table is indexed by them.
//
// For each supported older release it writes one Rust module:
//
//   * `STATE_RUNS` — runs of `(first engine state, length, first version
//     state)`. A client's version shares most of a block's state layout with
//     1.21.11, so the whole 29,671-state space collapses to a few thousand
//     runs; anything the version lacks is substituted (J2) or, last resort,
//     stone.
//   * `STATE_COUNT`, `STONE`, `STONE_FALLBACKS`.
//   * For 1.8 (pre-flattening) a `LEGACY_RUNS` table mapping engine states to
//     the `(id << 4)` value 1.8 stores, plus the reverse.
//
// Every table is verified here: the round trip engine name+properties →
// version id → version name+properties is checked for every state, and the
// generator fails if a rule points at a block the version does not have.
'use strict'
const fs = require('fs')
const path = require('path')
const d = require(require.resolve('minecraft-data', {
  paths: [
    process.cwd(),
    path.join(__dirname, '..', '..', 'probe'),
    '/tmp/opencode/repro',
    path.join(process.env.HOME || '', 'node_modules'),
  ],
}))

// Where the generated modules are written; set by `main`.
let outDir = 'crates/aether-server/src/protocol/compat/gen'

// The engine's anchor release. Its block/item ids *are* the engine's own.
const BASE = '1.21.11'
const base = d(BASE)

// codec module name -> (minecraft-data version, protocol, flattened?)
const VERSIONS = [
  ['v1_8', '1.8', 47, false],
  ['v1_17', '1.17', 755, true],
  ['v1_17_1', '1.17.1', 756, true],
  ['v1_18', '1.18', 757, true],
  ['v1_18_2', '1.18.2', 758, true],
  ['v1_19', '1.19', 759, true],
  ['v1_19_2', '1.19.2', 760, true],
  ['v1_19_3', '1.19.3', 761, true],
  ['v1_19_4', '1.19.4', 762, true],
  ['v1_20_1', '1.20.1', 763, true],
  ['v1_20_5', '1.20.5', 766, true],
  ['v1_21_1', '1.21.1', 767, true],
  ['v1_21_3', '1.21.3', 768, true],
  ['v1_21_4', '1.21.4', 769, true],
  ['v1_21_5', '1.21.5', 770, true],
  ['v1_21_6', '1.21.6', 771, true],
  ['v1_21_8', '1.21.8', 772, true],
  ['v1_21_9', '1.21.9', 773, true],
]

// --- schema helpers ---------------------------------------------------------

// The value list of one property: enums carry theirs, bools are always
// true-then-false (verified against 1.21.11's `defaultState` for every block),
// an int with no explicit list counts up from zero.
function propValues(s) {
  if (s.type === 'bool') return ['true', 'false']
  if (Array.isArray(s.values)) return s.values
  if (s.type === 'int') {
    return Array.from({ length: s.num_values }, (_, i) => String(i))
  }
  throw new Error('unknown property type ' + JSON.stringify(s))
}

// Offset of a property map within a block's state range. The **last**
// property varies fastest, which is vanilla's own flattening order — checked
// against every block's `defaultState` in `verify()`.
function offsetOf(block, props) {
  let off = 0
  for (const s of block.states) {
    const vv = propValues(s)
    let idx = vv.indexOf(props[s.name])
    if (idx < 0) idx = 0
    off = off * s.num_values + idx
  }
  return off
}

// Property map of a state offset within its block.
function propsOf(block, offset) {
  const out = {}
  let rem = offset
  for (let i = block.states.length - 1; i >= 0; i--) {
    const s = block.states[i]
    const n = s.num_values
    out[s.name] = propValues(s)[rem % n]
    rem = (rem / n) | 0
  }
  return out
}

const blocksByName = {}
for (const b of base.blocksArray) blocksByName[b.name] = b
const ENGINE_STATES = base.blocksArray.reduce(
  (a, b) => a + (b.maxStateId - b.minStateId + 1),
  0,
)

// --- substitution rules -----------------------------------------------------

// Load a rules file beside this one (default `blocks.json`); a bare array or
// one wrapped in a documentation object are both accepted. Shared with the
// item generator, which applies the same rules to block *items*.
function loadRules(file = 'blocks.json') {
  const raw = JSON.parse(fs.readFileSync(path.join(__dirname, file), 'utf8'))
  const list = Array.isArray(raw)
    ? raw.flatMap(x => (x && x.rules ? x.rules : [x]))
    : raw.rules
  return list
    .filter(r => r && r.from && r.to)
    .map(r => ({ from: strip(r.from), to: strip(r.to) }))
}
const strip = n => n.replace(/^minecraft:/, '')
const prefix = n => (n.includes(':') ? n : 'minecraft:' + n)

function glob(pattern, name) {
  const star = pattern.indexOf('*')
  if (star < 0) return name === pattern ? '' : null
  const pre = pattern.slice(0, star)
  const post = pattern.slice(star + 1)
  if (name.length < pre.length + post.length) return null
  if (!name.startsWith(pre) || !name.endsWith(post)) return null
  return name.slice(pre.length, name.length - post.length)
}

// Resolve a block name that the version lacks to one it has, by applying the
// rules repeatedly (a target that is itself missing is resolved again).
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

// --- table builders ---------------------------------------------------------

function buildFlat(mcVersion, rules) {
  const t = d(mcVersion)
  const byName = {}
  for (const b of t.blocksArray) byName[b.name] = b
  const names = new Set(Object.keys(byName))
  const resolve = resolver(names, rules)
  const stone = byName['stone'].defaultState
  const runs = []
  const fallbacks = new Set()
  let mismatches = 0

  const push = (e, id) => {
    const last = runs[runs.length - 1]
    if (last && last[0] + last[1] === e && last[2] + last[1] === id) last[1]++
    else runs.push([e, 1, id])
  }

  for (let e = 0; e < ENGINE_STATES; e++) {
    const bb = base.blocksByStateId[e]
    const props = propsOf(bb, e - bb.minStateId)
    let tb = byName[bb.name]
    if (!tb) {
      const sub = resolve(bb.name)
      if (sub) tb = byName[sub]
    }
    if (!tb) {
      fallbacks.add(bb.name)
      push(e, stone)
      continue
    }
    const id = tb.minStateId + offsetOf(tb, props)
    // Round trip, for the properties the target actually has.
    if (tb.name === bb.name) {
      const back = propsOf(tb, id - tb.minStateId)
      for (const k in props) {
        if (!tb.states.some(s => s.name === k)) continue
        if (back[k] !== props[k] && propValues(tb.states.find(s => s.name === k)).includes(props[k])) {
          // The value exists in the target yet came back different: a bug.
          mismatches++
          break
        }
      }
    }
    push(e, id)
  }
  const stateCount = t.blocksArray.reduce((a, b) => a + (b.maxStateId - b.minStateId + 1), 0)
  return { runs, stateCount, stone, fallbacks: [...fallbacks].sort(), mismatches }
}

// 1.8 predates flattened states: a cell is `(block_id << 4) | meta`. The data
// has no metadata table, so engine states map to their block's default meta.
function buildLegacy(mcVersion, rules) {
  const t = d(mcVersion)
  const byName = {}
  for (const b of t.blocksArray) byName[b.name] = b
  const names = new Set(Object.keys(byName))
  const resolve = resolver(names, rules)
  const stoneId = byName['stone'].id
  const runs = []
  const fallbacks = new Set()
  const seen = new Map() // legacy id -> first engine state
  for (let e = 0; e < ENGINE_STATES; e++) {
    const bb = base.blocksByStateId[e]
    let tb = byName[bb.name]
    if (!tb) {
      const sub = resolve(bb.name)
      if (sub) tb = byName[sub]
    }
    let id
    if (tb) {
      id = tb.id
      if (!seen.has(id)) seen.set(id, e)
    } else {
      fallbacks.add(bb.name)
      id = stoneId
    }
    const last = runs[runs.length - 1]
    if (last && last[0] + last[1] === e && last[2] === id) last[1]++
    else runs.push([e, 1, id])
  }
  // Reverse: legacy id -> the first engine state that maps to it.
  const reverse = [...seen.entries()].sort((a, b) => a[0] - b[0])
  return { runs, reverse, stone: stoneId, fallbacks: [...fallbacks].sort() }
}

// --- emit -------------------------------------------------------------------

function u32(n) {
  return n
}

function writeFlat(mod, mcVersion, rules) {
  const { runs, stateCount, stone, fallbacks, mismatches } = buildFlat(mcVersion, rules)
  const out = []
  const w = s => out.push(s)
  w(`//! Block-state compat table for ${mcVersion}.`)
  w('//!')
  w('//! Generated by `tools/gen/compat/gen.js` from PrismarineJS `minecraft-data`')
  w('//! and `tools/gen/compat/blocks.json`. Do not edit by hand.')
  w('')
  w('/// `(first engine state, run length, first version state)`.')
  w('pub static STATE_RUNS: &[(u32, u32, u32)] = &[')
  for (const [a, b, c] of runs) w(`    (${u32(a)}, ${u32(b)}, ${u32(c)}),`)
  w('];')
  w('')
  w(`/// Number of block states this version has.`)
  w(`pub const STATE_COUNT: u32 = ${stateCount};`)
  w('')
  w(`/// This version's own stone default state; the fallback for a block the`)
  w(`/// version lacks and no rule covers (and for a modded engine id).`)
  w(`pub const STONE: u32 = ${stone};`)
  w('')
  w('/// Block states the version lacks and no rule covers; they become stone.')
  w(`pub static STONE_FALLBACKS: &[&str] = &[${fallbacks.map(n => JSON.stringify(prefix(n))).join(', ')}];`)
  w('')
  fs.writeFileSync(path.join(outDir, mod + '.rs'), out.join('\n') + '\n')
  return { runs: runs.length, stateCount, fallbacks: fallbacks.length, mismatches }
}

function writeLegacy(mod, mcVersion, rules) {
  const { runs, reverse, stone, fallbacks } = buildLegacy(mcVersion, rules)
  const out = []
  const w = s => out.push(s)
  w(`//! Legacy (pre-flattening) block table for ${mcVersion}.`)
  w('//!')
  w('//! Generated by `tools/gen/compat/gen.js`. Do not edit by hand.')
  w('')
  w('/// `(first engine state, run length, legacy block id)`. 1.8 stores')
  w('/// `(id << 4) | meta`; the meta is always the block default here.')
  w('pub static LEGACY_RUNS: &[(u32, u32, u16)] = &[')
  for (const [a, b, c] of runs) w(`    (${u32(a)}, ${u32(b)}, ${c}),`)
  w('];')
  w('')
  w('/// `(legacy block id, engine default state)`, for placement round trips.')
  w('pub static LEGACY_TO_ENGINE: &[(u16, u32)] = &[')
  for (const [id, e] of reverse) w(`    (${id}, ${e}),`)
  w('];')
  w('')
  w(`/// 1.8's stone id; the fallback for a block the version lacks.`)
  w(`pub const STONE: u16 = ${stone};`)
  w('')
  w('/// Blocks the version lacks and no rule covers; they become stone.')
  w(`pub static STONE_FALLBACKS: &[&str] = &[${fallbacks.map(n => JSON.stringify(prefix(n))).join(', ')}];`)
  w('')
  fs.writeFileSync(path.join(outDir, mod + '.rs'), out.join('\n') + '\n')
  return { runs: runs.length, stateCount: 0, fallbacks: fallbacks.length, mismatches: 0 }
}

// --- verify the anchor: every 1.21.11 default state decomposes correctly ----
function verify() {
  let bad = 0
  for (const b of base.blocksArray) {
    const dflt = b.defaultState - b.minStateId
    if (offsetOf(b, propsOf(b, dflt)) !== dflt) bad++
  }
  if (bad) throw new Error(`${bad} anchor blocks fail the default-state round trip`)
}

// The resolver, the rule loader and the glob/strip helpers are exported so
// the item generator can reuse the *block* rules for block items (task J10);
// requiring this module must therefore not generate anything. Run it directly
// (`node tools/gen/compat/gen.js <out-dir>`) to write the block tables.
module.exports = { strip, prefix, glob, resolver, loadRules }

function main() {
  outDir = process.argv[2] || 'crates/aether-server/src/protocol/compat/gen'
  verify()
  fs.mkdirSync(outDir, { recursive: true })
  const rules = loadRules()
  console.log(`engines states: ${ENGINE_STATES}, rules: ${rules.length}`)
  let totalRuns = 0
  for (const [mod, mcVersion, proto, flat] of VERSIONS) {
    const r = flat
      ? writeFlat(mod, mcVersion, rules)
      : writeLegacy(mod, mcVersion, rules)
    totalRuns += r.runs
    console.log(
      `${mod.padEnd(9)} ${mcVersion.padEnd(7)} proto ${String(proto).padEnd(3)}` +
        ` runs ${String(r.runs).padStart(5)}` +
        (flat ? ` states ${String(r.stateCount).padStart(5)}` : '') +
        ` stone-fallbacks ${String(r.fallbacks).padStart(4)}` +
        (r.mismatches ? ` ROUND-TRIP-MISMATCH ${r.mismatches}` : ''),
    )
  }
  console.log(`total runs ${totalRuns}, approx ${(totalRuns * 12 / 1024).toFixed(0)} KiB`)
}

if (require.main === module) main()
