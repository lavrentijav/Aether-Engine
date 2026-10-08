const mineflayer = require('mineflayer')
const bot = mineflayer.createBot({ host:'127.0.0.1', port:25565, username:'StateProbe3',
  version:'1.21.11', auth:'offline', hideErrors:false })
bot.on('error', e=>console.log('ERR',e.message))
const sleep = ms => new Promise(r=>setTimeout(r,ms))

bot.once('spawn', async () => {
  await sleep(1500)
  const reg = bot.registry
  const give = n => { bot._client.write('set_creative_slot', { slot:36,
    item:{ itemCount:1, itemId:reg.itemsByName[n].id, addedComponentCount:0,
           removedComponentCount:0, components:[], removeComponents:[] }})
    bot._client.write('held_item_slot', { slotId: 0 }) }
  const place = (t, dir, cy, seq) => bot._client.write('block_place', {
    hand:0, location:{x:t.x,y:t.y,z:t.z}, direction:dir,
    cursorX:0.5, cursorY:cy, cursorZ:0.5, insideBlock:false, worldBorderHit:false, sequence:seq })
  const props = p => { const b = bot.blockAt(p); return b ? `${b.name} ${JSON.stringify(b.getProperties())}` : '?' }

  // Find dry solid ground next to the bot to build on.
  const base = bot.entity.position.floored()
  let ground = null
  for (let r = 2; r <= 40 && !ground; r++)
   for (let dx = -r; dx <= r && !ground; dx++) for (let dz = -r; dz <= r && !ground; dz++) {
    if (Math.max(Math.abs(dx), Math.abs(dz)) !== r) continue
    let g = null
    for (let y = 100; y > 40; y--) {
      const c = base.offset(dx, y - base.y, dz)
      const b = bot.blockAt(c)
      if (b && b.boundingBox === 'block' && b.name !== 'water') { g = c; break }
    }
    if (!g) continue
    const below = bot.blockAt(g), above = bot.blockAt(g.offset(0,1,0)), above2 = bot.blockAt(g.offset(0,2,0))
    if (below && below.boundingBox === 'block' && above && above.name === 'air' && above2 && above2.name === 'air') ground = g
  }
  if (!ground) { console.log('no dry ground found near', base.toString()); process.exit(1) }
  console.log('building on', ground.toString(), '(', props(ground), ')')

  console.log('\n--- 1. does a stair follow where the player looks? ---')
  give('oak_stairs'); await sleep(250)
  let i = 0
  for (const yaw of [0, Math.PI/2, Math.PI, 3*Math.PI/2]) {
    await bot.look(yaw, 0.8, true)
    await sleep(150)
    const t = ground.offset(0, 0, i - 2)
    place(t, 1, 1.0, 500 + i)
    await sleep(400)
    const b = bot.blockAt(t.offset(0,1,0))
    console.log(`   look yaw=${(yaw*180/Math.PI).toFixed(0).padStart(3)}deg -> ` +
                `${b ? b.name : '?'} facing=${b && b.getProperties ? b.getProperties().facing : '?'}`)
    i++
  }

  console.log('\n--- 2. does clicking a bottom face make a top slab? ---')
  give('oak_slab'); await sleep(250)
  place(ground, 1, 1.0, 510); await sleep(350)
  console.log('   clicked the TOP face   ->', props(ground.offset(0,1,0)))
  place(ground.offset(0,1,0), 0, 0.0, 511); await sleep(350)
  console.log('   clicked a BOTTOM face  ->', props(ground.offset(0,0,0)), '(expected: a top slab above)')

  console.log('\n--- 3. is a door two linked blocks? ---')
  give('oak_door'); await sleep(250)
  const d = ground.offset(1, 0, 0)
  place(d, 1, 1.0, 520); await sleep(400)
  console.log('   lower =', props(d.offset(0,1,0)))
  console.log('   upper =', props(d.offset(0,2,0)), ' <- vanilla puts half=upper here')

  console.log('\n--- 4. do two adjacent fences connect? ---')
  give('oak_fence'); await sleep(250)
  const f = ground.offset(2, 0, 0)
  place(f, 1, 1.0, 530); await sleep(300)
  place(f.offset(1,0,0), 1, 1.0, 531); await sleep(400)
  console.log('   fence A =', props(f.offset(0,1,0)))
  console.log('   fence B =', props(f.offset(1,1,0)))
  process.exit(0)
})
setTimeout(()=>{console.log('TIMEOUT');process.exit(1)}, 70000)
