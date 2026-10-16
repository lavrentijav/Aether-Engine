// Builds one per-version compat resource pack: the pack that draws the real
// picture of a block on the **carrier states** the compat layer sends for it,
// while leaving every other carrier state vanilla (task RP1, Part B).
//
//   node tools/pack/build.js --release 1.20.1 --jar <client.jar> --out <dir>
//   node tools/pack/build.js --release 1.20.1 --stub-texture --out /tmp/p
//
// It reads the checked-in assignments (`tools/gen/compat/pack.json`) and the
// vanilla texture for each disguised block out of the operator's own client
// jar. **No vanilla asset is committed**: the jar is read at build/deploy time,
// and the output directory is meant to be served, not checked in.
//
// Output (pack root, ready to zip):
//   pack.mcmeta
//   assets/minecraft/blockstates/<carrier>.json   (all vanilla variants + overrides)
//   assets/aether/models/block/<block>.json        (cube_all with the texture)
//   assets/aether/textures/block/<block>.png
//
// The blockstates file must carry *every* state of the carrier, not just the
// overridden ones: replacing the file replaces vanilla's, so a missing variant
// would render a real note block as the missing model. Every non-overridden
// variant points back at the vanilla model.
'use strict'
const fs = require('fs')
const path = require('path')
const zlib = require('zlib')
const gen = require('../gen/compat/gen_pack.js')

// --- arguments --------------------------------------------------------------
function parseArgs(argv) {
  const out = {}
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a.startsWith('--')) out[a.slice(2)] = argv[i + 1]?.startsWith('--') ? true : argv[++i]
  }
  return out
}
const args = parseArgs(process.argv.slice(2))
const release = args.release || '1.20.1'
const outDir = args.out || path.join('packs', 'compat', release)
const stub = !!args['stub-texture']
const textureOverride = typeof args.texture === 'string' ? args.texture : null
const jar = typeof args.jar === 'string' ? args.jar : null
if (!stub && !textureOverride && !jar) {
  console.error('need --jar <client.jar>, --texture <png> or --stub-texture')
  process.exit(2)
}

const assignments = gen.loadAssignments().filter(a => a.release === release)
if (assignments.length === 0) {
  console.error(`no carrier assignments for ${release} in pack.json`)
  process.exit(2)
}

// A 16x16 placeholder so the pipeline is testable without a game jar. The real
// texture always comes from --texture or the jar.
const STUB_PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAABKElEQVR42hXMwYBFIQAAwIfwEUIIIYQQQgghhBBCCCGEEELIYHc6zHW+7/v7+xGIJDKFSqMzmCw2h8v3/QQEIolModLoDCaLzeH+XhAEBCKJTKHS6Awmi83hhhdEAYFIIlOoNDqDyWJzuPEFSUAgksgUKo3OYLLYHG56QRYQiCQyhUqjM5gsNoebX1AEBCKJTKHS6Awmi83hlhdUAYFIIlOoNDqDyWJzuPUFTUAgksgUKo3OYLLYHG57QRcQiCQyhUqjM5gsNofbXzAEBCKJTKHS6Awmi83hjhdMAYFIIlOoNDqDyWJzuPMFS0AgksgUKo3OYLLYHO56wRYQiCQyhUqjM5gsNoe7X3AEBCKJTKHS6Awmi83hnhdcAYFIIlOoNDqDyWJzuPwDFenuH+4UurQAAAAASUVORK5CYII=',
  'base64',
)

// --- minimal zip reader (stored + deflate), for pulling textures from a jar --
function readZipEntry(zipPath, wanted) {
  const buf = fs.readFileSync(zipPath)
  // Find the End Of Central Directory record (signature 0x06054b50), scanning
  // back over the optional comment.
  let eocd = -1
  for (let i = buf.length - 22; i >= 0 && i > buf.length - 22 - 0xffff; i--) {
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocd = i
      break
    }
  }
  if (eocd < 0) throw new Error('not a zip: no end-of-central-directory')
  const count = buf.readUInt16LE(eocd + 10)
  let p = buf.readUInt32LE(eocd + 16)
  for (let n = 0; n < count; n++) {
    if (buf.readUInt32LE(p) !== 0x02014b50) throw new Error('bad central directory')
    const method = buf.readUInt16LE(p + 10)
    const compSize = buf.readUInt32LE(p + 20)
    const nameLen = buf.readUInt16LE(p + 28)
    const extraLen = buf.readUInt16LE(p + 30)
    const commentLen = buf.readUInt16LE(p + 32)
    const localOff = buf.readUInt32LE(p + 42)
    const name = buf.toString('utf8', p + 46, p + 46 + nameLen)
    if (name === wanted) {
      const lh = localOff
      const lnameLen = buf.readUInt16LE(lh + 26)
      const lextraLen = buf.readUInt16LE(lh + 28)
      const start = lh + 30 + lnameLen + lextraLen
      const data = buf.subarray(start, start + compSize)
      return method === 0 ? data : zlib.inflateRawSync(data)
    }
    p += 46 + nameLen + extraLen + commentLen
  }
  return null
}

// The vanilla blockstates file for a carrier, straight out of the jar, so the
// variant keys are the client's own. `null` when there is no jar or no entry.
function jarBlockstates(carrierName) {
  const data = readZipEntry(jar, `assets/minecraft/blockstates/${carrierName}.json`)
  if (!data) return null
  const parsed = JSON.parse(data.toString('utf8'))
  // A multipart blockstates file is left to the synthesis path; the carriers
  // this layer uses are single-variant maps.
  return parsed.variants && !Array.isArray(parsed.variants) ? parsed.variants : null
}

function textureFor(blockName) {
  if (stub) return STUB_PNG
  if (textureOverride) return fs.readFileSync(textureOverride)
  const entry = `assets/minecraft/textures/block/${blockName}.png`
  const data = readZipEntry(jar, entry)
  if (!data) {
    throw new Error(
      `${entry} not in ${jar}. A block whose texture name differs from its ` +
        `block name needs --texture or a per-block override.`,
    )
  }
  return data
}

// --- blockstates ------------------------------------------------------------
function variantKey(block, props) {
  return block.states.map(s => `${s.name}=${props[s.name]}`).join(',')
}

// The vanilla model a carrier's non-overridden states keep. Note blocks are a
// plain cube_all in every release this server speaks.
const CARRIER_MODEL = { note_block: 'minecraft:block/note_block' }

// The release's own carrier block and every one of its states, so the variant
// keys match the blockstates schema the client validates against.
function carrierStatesForRelease(release, carrierName) {
  const block = gen.releaseData(release).blocksByName[carrierName]
  if (!block) throw new Error(`${release} has no carrier ${carrierName}`)
  const out = []
  const n = block.maxStateId - block.minStateId + 1
  for (let off = 0; off < n; off++) {
    const props = {}
    let rem = off
    for (let i = block.states.length - 1; i >= 0; i--) {
      const s = block.states[i]
      const vals = gen.propValues(s)
      props[s.name] = vals[rem % s.num_values]
      rem = (rem / s.num_values) | 0
    }
    out.push({ key: variantKey(block, props), model: CARRIER_MODEL[carrierName] || `minecraft:block/${carrierName}` })
  }
  return { block, states: out }
}

function writeFile(rel, contents) {
  const full = path.join(outDir, rel)
  fs.mkdirSync(path.dirname(full), { recursive: true })
  fs.writeFileSync(full, contents)
}

let packFormat = assignments[0].packFormat
writeFile('pack.mcmeta', JSON.stringify({
  pack: { pack_format: packFormat, description: `Aether compat stand-ins (${release})` },
}, null, 2) + '\n')

for (const a of assignments) {
  const expanded = gen.expand(release, a)
  const carrierName = gen.strip(a.carrier)
  const model = `aether:block/${expanded.blockName}`

  const texture = textureFor(expanded.blockName)
  writeFile(`assets/aether/textures/block/${expanded.blockName}.png`, texture)

  writeFile(`assets/aether/models/block/${expanded.blockName}.json`, JSON.stringify({
    parent: 'minecraft:block/cube_all',
    textures: { all: `aether:block/${expanded.blockName}` },
  }, null, 2) + '\n')

  // All vanilla variants, with the disguised ones pointed at our model. The
  // jar's own blockstates file is authoritative when we have it (its keys are
  // the ones the client validates against); otherwise the variant set is
  // synthesised from PrismarineJS data, which is right for note blocks.
  const { block: relBlock, states: relStates } = carrierStatesForRelease(release, carrierName)
  const overrideByKey = new Map(
    expanded.states.map(s => [variantKey(relBlock, s.props), model]),
  )
  const fromJar = jar ? jarBlockstates(carrierName) : null
  const variants = fromJar || {}
  if (!fromJar || Object.keys(variants).length === 0) {
    for (const st of relStates) variants[st.key] = { model: st.model }
  }
  for (const [key, override] of overrideByKey) {
    if (!(key in variants)) throw new Error(`carrier state ${key} not a variant of ${carrierName}`)
    variants[key] = { model: override }
  }
  writeFile(`assets/minecraft/blockstates/${carrierName}.json`,
    JSON.stringify({ variants }, null, 2) + '\n')
  console.log(`${expanded.block} -> ${carrierName} (${expanded.states.length} state(s))`)
}

console.log(`pack written to ${outDir} (pack_format ${packFormat})`)
console.log(
  'zip the directory and serve it; set [compat.pack] url, hash and ' +
    `pack_format = ${packFormat} in the server config (the format must match or ` +
    'the pack is not offered).',
)
