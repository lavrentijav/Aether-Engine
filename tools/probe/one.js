const mineflayer = require('mineflayer')
const bot = mineflayer.createBot({ host:'127.0.0.1', port:25565, username:'OneProbe', version:'1.21.11', auth:'offline' })
const sleep = ms => new Promise(r=>setTimeout(r,ms))
bot.on('error', e=>console.log('ERR',e.message))
bot._client.on('block_update', d => console.log('  block_update', JSON.stringify(d.location), 'state', d.type))
bot._client.on('acknowledge_player_digging', d => console.log('  ack', JSON.stringify(d)))
bot.once('spawn', async () => {
  await sleep(1500)
  const reg = bot.registry, base = bot.entity.position.floored()
  console.log('at', base.toString())
  let seq = 900
  for (const name of ['stone','oak_fence','oak_fence','glass_pane']) {
    const item = reg.itemsByName[name]
    bot._client.write('set_creative_slot', { slot:36, item:{ itemCount:1, itemId:item.id, addedComponentCount:0, removedComponentCount:0, components:[], removeComponents:[] } })
    bot._client.write('held_item_slot', { slotId:0 })
    await sleep(250)
    const t = base.offset(name==='stone'?5:(name==='glass_pane'?9:6+(seq-901)),-1,0)
    console.log('place', name, 'clicking', t.toString())
    bot._client.write('block_place', { hand:0, location:{x:t.x,y:t.y,z:t.z}, direction:1, cursorX:0.5, cursorY:1.0, cursorZ:0.5, insideBlock:false, worldBorderHit:false, sequence:seq++ })
    await sleep(500)
    const b = bot.blockAt(t.offset(0,1,0))
    console.log('  ->', b&&b.name, JSON.stringify(b&&b.getProperties?b.getProperties():{}))
  }
  process.exit(0)
})
setTimeout(()=>{console.log('TIMEOUT');process.exit(1)},60000)
