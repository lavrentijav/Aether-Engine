// Can a player still brick themselves inside a block?
const mineflayer = require('mineflayer')
const { Vec3 } = require('vec3')
const bot = mineflayer.createBot({ host:'127.0.0.1', port:25565, username:'SelfProbe', version:'1.21.11', auth:'offline' })
const sleep = ms => new Promise(r=>setTimeout(r,ms))
bot.on('error', e=>console.log('ERR', e.message))
bot.once('spawn', async () => {
  await sleep(22000)
  const reg = bot.registry, item = reg.itemsByName['stone']
  bot._client.write('set_creative_slot', { slot:36, item:{ itemCount:1, itemId:item.id, addedComponentCount:0, removedComponentCount:0, components:[], removeComponents:[] } })
  bot._client.write('held_item_slot', { slotId:0 })
  await sleep(400)
  const p = bot.entity.position.floored()
  const cases = [
    ['inside their feet', p.offset(0,0,0)],
    ['inside their head', p.offset(0,1,0)],
    ['at their feet (below)', p.offset(0,-1,0)],
    ['well clear of them', p.offset(4,0,0)],
  ]
  let seq = 1200
  for (const [what, cell] of cases) {
    // click the cell below, top face -> the server places into `cell`
    const t = cell.offset(0,-1,0)
    bot._client.write('block_place', { hand:0, location:{x:t.x,y:t.y,z:t.z}, direction:1, cursorX:0.5, cursorY:1.0, cursorZ:0.5, insideBlock:false, worldBorderHit:false, sequence:seq++ })
    await sleep(700)
    const b = bot.blockAt(cell)
    console.log(`${what.padEnd(22)} -> ${b?b.name:'?'}`)
  }
  process.exit(0)
})
setTimeout(()=>{console.log('TIMEOUT');process.exit(1)},90000)
