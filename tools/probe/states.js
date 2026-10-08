// Place state-carrying blocks and report the state the SERVER echoes back.
const mineflayer = require('mineflayer')
const { Vec3 } = require('vec3')

const bot = mineflayer.createBot({
  host: '127.0.0.1', port: 25565, username: 'StateProbe',
  version: '1.21.11', auth: 'offline', hideErrors: false,
})
bot.on('error', e => console.log('ERR', e.message))
bot.on('kicked', r => console.log('KICK', JSON.stringify(r).slice(0,300)))

const sleep = ms => new Promise(r => setTimeout(r, ms))

bot.once('spawn', async () => {
  await sleep(1500)
  const reg = bot.registry
  const base = bot.entity.position.floored()
  console.log('at', base.toString())

  // Ask the server for each block by putting it in a creative slot, then place
  // it and read back what the server says is there.
  const trials = [
    ['oak_stairs',   'facing / half / shape'],
    ['oak_fence',    'north/east/south/west connections'],
    ['glass_pane',   'connections'],
    ['oak_door',     'two linked blocks, half=lower/upper'],
    ['oak_slab',     'type=bottom/top/double'],
    ['oak_log',      'axis=x/y/z'],
    ['chest',        'facing'],
    ['redstone_wire','power + connections'],
  ]

  let n = 0
  for (const [name, what] of trials) {
    const blk = reg.blocksByName[name]
    const item = reg.itemsByName[name]
    if (!blk || !item) { console.log(`SKIP ${name}`); continue }

    // Put the item in the hotbar via the creative slot packet.
    bot._client.write('set_creative_slot', {
      slot: 36,
      item: { itemCount: 1, itemId: item.id, addedComponentCount: 0, removedComponentCount: 0, components: [], removeComponents: [] },
    })
    bot._client.write('held_item_slot', { slotId: 0 })
    await sleep(200)

    // Place against the ground, a few blocks apart so nothing interferes.
    const target = base.offset(2 + n * 2, -1, 0)
    const where = target.offset(0, 1, 0)
    bot._client.write('block_place', {
      hand: 0,
      location: { x: target.x, y: target.y, z: target.z },
      direction: 1,          // top face
      cursorX: 0.5, cursorY: 1.0, cursorZ: 0.5,
      insideBlock: false,
      worldBorderHit: false,
      sequence: 100 + n,
    })
    await sleep(400)

    const got = bot.blockAt(where)
    const props = got && got.getProperties ? got.getProperties() : {}
    const def = blk.defaultState
    console.log(
      `${name.padEnd(14)} want:${what}\n` +
      `   got name=${got ? got.name : '?'} stateId=${got ? got.stateId : '?'} ` +
      `(block default=${def}) props=${JSON.stringify(props)}`
    )
    n++
  }

  // Waterlogging: place a slab, then water on it.
  console.log('\n-- waterlogging --')
  const slab = reg.itemsByName['oak_slab']
  const water = reg.itemsByName['water_bucket']
  console.log('water_bucket item exists:', !!water)

  process.exit(0)
})
setTimeout(() => { console.log('TIMEOUT'); process.exit(1) }, 60000)
