// Verify the block-update pass against a real client: build a fence line and a
// wall corner, and read back what the SERVER says the older blocks became.
const mineflayer = require('mineflayer')
const bot = mineflayer.createBot({
  host: '127.0.0.1', port: 25565, username: 'NeighProbe',
  version: '1.21.11', auth: 'offline', hideErrors: false,
})
bot.on('error', e => console.log('ERR', e.message))
bot.on('kicked', r => console.log('KICK', JSON.stringify(r).slice(0, 300)))
const sleep = ms => new Promise(r => setTimeout(r, ms))

let seq = 500
async function place(item, target, dir = 1) {
  bot._client.write('set_creative_slot', {
    slot: 36,
    item: { itemCount: 1, itemId: item.id, addedComponentCount: 0, removedComponentCount: 0, components: [], removeComponents: [] },
  })
  bot._client.write('held_item_slot', { slotId: 0 })
  await sleep(150)
  bot._client.write('block_place', {
    hand: 0,
    location: { x: target.x, y: target.y, z: target.z },
    direction: dir, cursorX: 0.5, cursorY: 1.0, cursorZ: 0.5,
    insideBlock: false, worldBorderHit: false, sequence: seq++,
  })
  await sleep(350)
}

bot.once('spawn', async () => {
  await sleep(30000) // let the initial chunk stream finish first
  const reg = bot.registry
  // Build in clear air well above the terrain: the server derives the
  // placement cell from the clicked face, so nothing has to be solid, and
  // this keeps the test off whatever the generator happened to put at spawn.
  const base = bot.entity.position.floored()
  bot._client.on('block_update', d => console.log('   srv block_update', JSON.stringify(d.location), d.type))
  console.log('building at', base.toString())
  const fence = reg.itemsByName['oak_fence']
  const wall = reg.itemsByName['cobblestone_wall']
  const show = (label, v) => {
    const b = bot.blockAt(v)
    console.log(`${label} ${b ? b.name : '?'} ${JSON.stringify(b && b.getProperties ? b.getProperties() : {})}`)
  }

  // Two fences side by side, placed one after the other on top of the ground.
  const g1 = base.offset(2, -1, 0), g2 = base.offset(3, -1, 0)
  await place(fence, g1)
  await place(fence, g2)
  console.log('-- fence line --')
  show('first ', g1.offset(0, 1, 0))
  show('second', g2.offset(0, 1, 0))

  // A wall corner: straight run first, then bend it.
  const w1 = base.offset(2, -1, 4), w2 = base.offset(3, -1, 4), w3 = base.offset(3, -1, 5)
  await place(wall, w1); await place(wall, w2); await place(wall, w3)
  console.log('-- wall corner --')
  show('corner', w2.offset(0, 1, 0))
  show('arm-x ', w1.offset(0, 1, 0))
  show('arm-z ', w3.offset(0, 1, 0))
  process.exit(0)
})
setTimeout(() => { console.log('TIMEOUT'); process.exit(1) }, 60000)
