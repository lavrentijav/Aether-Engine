// Generates the per-release **carrier** tables of the compat resource pack
// (task RP1, Part B).
//
//   node tools/gen/compat/gen_pack.js <out-dir>
//
// The assignments are the checked-in, stable `pack.json` beside this file: one
// entry says "on release R, show engine block B on carrier C in state P". The
// generator expands B to *all* of its engine states, bumping the carrier's
// `note` (or equivalent) per state, and writes `compat/pack_gen/<release>.rs`
// mapping each engine state to the engine state of the carrier to show.
//
// Why a carrier *engine* state and not the release's own id: the codec already
// translates an engine state to each release's wire state through the block
// compat layer (J1/J2). If the carrier state is chosen with properties the
// release shares, that same translation lands on the matching state there — so
// the pack half needs no knowledge of any codec, and `compat/gen/*` stays
// byte-stable.
//
// The release module is only written when a release has at least one
// assignment; the server gates the pack offer on the derived table (see
// `compat::pack::table_for`), so a release with no assignments keeps the plain
// stand-in.
//
// It is required as a module by `tools/pack/build.js`, which needs the same
// expansion to know which carrier states to retexture.
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

const outDir = process.argv[2] || 'crates/aether-server/src/protocol/compat/pack_gen'

// The engine's anchor release: its block-state ids *are* the engine's own.
const BASE = '1.21.11'
const base = d(BASE)

// The value list of one property (bool is true-then-false; int counts from 0;
// an enum carries its own list) — the same layout `gen.js` uses.
function propValues(s) {
  if (s.type === 'bool') return ['true', 'false']
  if (Array.isArray(s.values)) return s.values
  if (s.type === 'int') return Array.from({ length: s.num_values }, (_, i) => String(i))
  throw new Error('unknown property type ' + JSON.stringify(s))
}

// Offset of a property map within a block's state range, in vanilla's
// flattening order (the **last** property varies fastest).
function stateOf(block, props) {
  let off = 0
  for (const s of block.states) {
    const vals = propValues(s)
    let idx = vals.indexOf(String(props[s.name]))
    if (idx < 0) idx = 0
    off = off * s.num_values + idx
  }
  return block.minStateId + off
}

const strip = n => n.replace(/^minecraft:/, '')
const modName = release => 'v' + release.replace(/\./g, '_')

function loadAssignments() {
  const raw = JSON.parse(fs.readFileSync(path.join(__dirname, 'pack.json'), 'utf8'))
  return raw.assignments || []
}

// `note` (or the first int property) is bumped per source state so a
// multi-state block gets distinct carrier states; a single-state block keeps
// the base value.
function carrierFor(assignment, stateIndex) {
  const carrier = base.blocksByName[strip(assignment.carrier)]
  if (!carrier) throw new Error(`unknown carrier ${assignment.carrier} in the engine`)
  const props = { ...assignment.carrierProps }
  if (stateIndex > 0) {
    const bump = carrier.states.find(s => s.type === 'int')
    if (!bump) {
      throw new Error(
        `${assignment.block} has multiple states but carrier ${assignment.carrier} has no int property to vary`,
      )
    }
    const next = Number(props[bump.name] ?? 0) + stateIndex
    if (next >= bump.num_values) {
      throw new Error(
        `${assignment.carrier}: ${stateIndex + 1} states needed but \`${bump.name}\` has only ${bump.num_values} values`,
      )
    }
    props[bump.name] = next
  }
  for (const key of Object.keys(props)) {
    if (carrier.states.some(s => s.name === key)) continue
    throw new Error(`carrier ${assignment.carrier} has no property ${key}`)
  }
  return { props, id: stateOf(carrier, props) }
}

// Confirm the carrier props survive the codec's name+properties translation:
// the target release must name the carrier block and have each property with
// the same value. Runs the same check the block tables rest on, here so a bad
// assignment fails at generation rather than as a wrong block on a client.
function checkTranslatable(release, assignment, carrier) {
  const t = d(release)
  const tb = t.blocksByName[strip(assignment.carrier)]
  if (!tb) throw new Error(`${release} has no carrier ${assignment.carrier}`)
  for (const [key, value] of Object.entries(carrier.props)) {
    const s = tb.states.find(s => s.name === key)
    if (!s) throw new Error(`${release} carrier ${assignment.carrier} has no property ${key}`)
    if (!propValues(s).includes(String(value))) {
      throw new Error(`${release} carrier ${assignment.carrier}.${key} has no value ${value}`)
    }
  }
}

// Expand one assignment to its engine-state pairs and, for the pack builder,
// the per-state carrier properties.
function expand(release, assignment) {
  const block = base.blocksByName[strip(assignment.block)]
  if (!block) throw new Error(`unknown engine block ${assignment.block}`)
  const stateCount = block.maxStateId - block.minStateId + 1
  const states = []
  for (let i = 0; i < stateCount; i++) {
    const carrier = carrierFor(assignment, i)
    checkTranslatable(release, assignment, carrier)
    states.push({ index: i, props: carrier.props, carrierId: carrier.id, engineState: block.minStateId + i })
  }
  return {
    block: assignment.block,
    blockName: strip(assignment.block),
    carrier: assignment.carrier,
    packFormat: assignment.packFormat,
    states,
  }
}

function buildRelease(release, assignments) {
  const pairs = []
  const seenCarrier = new Map()
  const canonical = new Map() // carrier engine state -> canonical engine state
  const canonicalOf = new Map() // carrier block name -> canonical engine state
  let packFormat
  for (const a of assignments) {
    const expanded = expand(release, a)
    if (packFormat === undefined) packFormat = expanded.packFormat
    const carrierBlock = base.blocksByName[strip(a.carrier)]
    const canon = carrierBlock.defaultState
    canonicalOf.set(carrierBlock.name, canon)
    for (const s of expanded.states) {
      const prev = seenCarrier.get(s.carrierId)
      if (prev !== undefined && prev !== s.engineState) {
        throw new Error(
          `${release}: carrier state ${s.carrierId} assigned to two blocks (${prev}, ${s.engineState})`,
        )
      }
      if (s.carrierId === canon) {
        throw new Error(
          `${release}: ${a.block} is disguised on the carrier's *canonical* state ` +
            `${canon}, which is left vanilla — real ${a.carrier} would render as ` +
            `${a.block}. Use a non-canonical carrier state.`,
        )
      }
      seenCarrier.set(s.carrierId, s.engineState)
      pairs.push([s.engineState, s.carrierId])
    }
  }
  // Every *state* of every carrier maps to that carrier's canonical state, so a
  // genuine carrier block (a tuned note, a powered one) is never sent in the
  // state a disguise occupies. The engine keeps the true state; only the wire
  // is canonicalised, and the server plays the sound.
  for (const [carrierName, canon] of canonicalOf) {
    const carrierBlock = base.blocksByName[carrierName]
    for (let id = carrierBlock.minStateId; id <= carrierBlock.maxStateId; id++) {
      if (id !== canon && !canonical.has(id)) canonical.set(id, canon)
    }
  }
  pairs.sort((x, y) => x[0] - y[0])
  const canonicalPairs = [...canonical.entries()].sort((x, y) => x[0] - y[0])
  if (packFormat === undefined) throw new Error(`${release}: no assignment declares a packFormat`)
  return { pairs, canonicalPairs, packFormat }
}

function writeRelease(release, { pairs, canonicalPairs, packFormat }) {
  const out = []
  const w = s => out.push(s)
  w(`//! Carrier table for the compat resource pack on ${release}.`)
  w('//!')
  w('//! Generated by `tools/gen/compat/gen_pack.js` from `pack.json`.')
  w('//! Do not edit by hand. See `docs/design/compat-resource-pack.md`.')
  w('')
  w('/// The resource-pack `pack_format` a client of this release needs; the')
  w('/// pack offered to it must declare this same format.')
  w(`pub const PACK_FORMAT: u32 = ${packFormat};`)
  w('')
  w('/// `(engine block state, carrier engine block state)`, sorted by the')
  w('/// first. The codec translates the carrier engine state to this release')
  w('/// just as it would any other engine block.')
  w(`pub static CARRIERS: &[(u32, u32)] = &[`)
  for (const [from, to] of pairs) w(`    (${from}, ${to}),`)
  w('];')
  w('')
  w('/// `(engine carrier state, canonical engine state)`, sorted by the first.')
  w('/// A genuine carrier block is shown as this canonical (vanilla-model)')
  w('/// state on a carrier-enabled client, whatever its real state, so it can')
  w('/// never be mistaken for a disguise; the server keeps the true state.')
  w(`pub static CANONICAL: &[(u32, u32)] = &[`)
  for (const [from, to] of canonicalPairs) w(`    (${from}, ${to}),`)
  w('];')
  w('')
  fs.mkdirSync(outDir, { recursive: true })
  fs.writeFileSync(path.join(outDir, modName(release) + '.rs'), out.join('\n') + '\n')
}

function main() {
  const assignments = loadAssignments()
  const byRelease = new Map()
  for (const a of assignments) {
    if (!byRelease.has(a.release)) byRelease.set(a.release, [])
    byRelease.get(a.release).push(a)
  }
  if (byRelease.size === 0) {
    console.log('no carrier assignments in pack.json; nothing to generate')
  }
  for (const [release, list] of byRelease) {
    const built = buildRelease(release, list)
    writeRelease(release, built)
    console.log(
      `${modName(release).padEnd(9)} ${release.padEnd(7)} proto ${String(list[0].protocol).padEnd(3)}` +
        ` carriers ${String(built.pairs.length).padStart(5)}` +
        ` canonical ${String(built.canonicalPairs.length).padStart(5)}` +
        ` pack_format ${built.packFormat}`,
    )
  }
}

if (require.main === module) main()

// The minecraft-data release object, for the pack builder's blockstates keys.
const releaseData = release => d(release)

module.exports = { loadAssignments, expand, buildRelease, modName, strip, propValues, releaseData, base }
