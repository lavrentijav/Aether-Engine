// Second survival pass: chest storage, furnace smelting, eating, fall
// damage, death and respawn. Needs an operator account (`/give`, `/tp`).
const mineflayer = require('mineflayer')
const bot = mineflayer.createBot({ host: '127.0.0.1', port: 25565, username: process.argv[2] || 'Probe2', version: '1.21.11', auth: 'offline' })
const sleep = ms => new Promise(r => setTimeout(r, ms))
const results = []
const check = (name, ok, extra = '') => { results.push([name, ok]); console.log(ok ? 'PASS' : 'FAIL', name, extra) }
const countOf = n => bot.inventory.items().filter(i => i.name === n).reduce((a, i) => a + i.count, 0)
bot.on('error', e => console.log('ERR', e.message))
bot.on('kicked', r => console.log('KICKED', JSON.stringify(r)))
bot.once('spawn', async () => {
  try {
    await sleep(1500)
    bot.chat('/tp 27 70 37'); await sleep(2500)
    const ground = bot.blockAt(bot.entity.position.offset(0, -1, 0))
    console.log('standing on', ground && ground.name, bot.entity.position.toString())
    for (const [item, n] of [['chest', 1], ['furnace', 1], ['raw_iron', 3], ['coal', 2], ['bread', 4], ['cobblestone', 10]]) bot.chat(`/give ${item} ${n}`)
    await sleep(1200)
    // Place a chest next to us.
    const at = bot.entity.position.floored()
    const base = bot.blockAt(at.offset(2, -1, 0))
    await bot.equip(bot.inventory.items().find(i => i.name === 'chest'), 'hand')
    await bot.placeBlock(base, new (require('vec3').Vec3)(0, 1, 0)).catch(e => console.log('place', e.message))
    await sleep(600)
    const chestBlock = bot.blockAt(at.offset(2, 0, 0))
    check('chest placed', chestBlock && chestBlock.name === 'chest', chestBlock && chestBlock.name)
    check('chest consumed', countOf('chest') === 0)
    if (chestBlock && chestBlock.name === 'chest') {
      const chest = await bot.openContainer(chestBlock)
      await chest.deposit(bot.registry.itemsByName.cobblestone.id, null, 6)
      await sleep(400)
      check('chest holds deposit', chest.containerItems().some(i => i.name === 'cobblestone' && i.count === 6))
      chest.close()
      await sleep(400)
      const again = await bot.openContainer(chestBlock)
      check('chest persists', again.containerItems().some(i => i.name === 'cobblestone' && i.count === 6))
      await again.withdraw(bot.registry.itemsByName.cobblestone.id, null, 6)
      again.close()
      await sleep(400)
      check('chest withdraw', countOf('cobblestone') >= 10, countOf('cobblestone'))
    }
    // Furnace: smelt raw iron with coal.
    // Solid ground with air above, a couple of blocks away.
    let base2 = null
    for (const [dx, dz] of [[0, 2], [0, -2], [-2, 0], [2, 2], [-2, -2]]) {
      for (let dy = 1; dy >= -3 && !base2; dy--) {
        const b = bot.blockAt(at.offset(dx, dy, dz)), up = bot.blockAt(at.offset(dx, dy + 1, dz))
        if (b && b.boundingBox === 'block' && up && up.name === 'air') base2 = b
      }
      if (base2) break
    }
    await bot.equip(bot.inventory.items().find(i => i.name === 'furnace'), 'hand')
    await bot.placeBlock(base2, new (require('vec3').Vec3)(0, 1, 0)).catch(e => console.log('place', e.message))
    await sleep(600)
    const fb = bot.blockAt(base2.position.offset(0, 1, 0))
    check('furnace placed', fb && fb.name === 'furnace', fb && fb.name)
    if (fb && fb.name === 'furnace') {
      const f = await bot.openFurnace(fb)
      await f.putFuel(bot.registry.itemsByName.coal.id, null, 1)
      await f.putInput(bot.registry.itemsByName.raw_iron.id, null, 1)
      await sleep(11500)
      const out = f.outputItem()
      check('furnace smelts', out && out.name === 'iron_ingot', out && out.name)
      if (out) await f.takeOutput()
      f.close()
    }
    // Fall damage: tp up 10 blocks.
    const hp0 = bot.health
    const p = bot.entity.position
    bot.chat(`/tp ${p.x} ${p.y + 12} ${p.z}`)
    await sleep(3500)
    check('fall damage', bot.health < hp0, `${hp0} -> ${bot.health}`)
    // Eat bread.
    const food0 = bot.food
    await bot.equip(bot.inventory.items().find(i => i.name === 'bread'), 'hand')
    if (bot.food < 20) {
      await bot.consume().catch(e => console.log('consume', e.message))
      await sleep(500)
      check('eating restores food', bot.food > food0 || countOf('bread') < 4, `${food0} -> ${bot.food}`)
    } else console.log('not hungry, food', bot.food)
    // Death and respawn.
    let died = false
    bot.once('death', () => { died = true })
    bot.chat('/kill')
    await sleep(2000)
    check('death', died)
    await sleep(2500)
    check('respawned with full health', bot.health === 20, bot.health)
    check('inventory dropped on death', countOf('cobblestone') === 0 && countOf('bread') === 0, bot.inventory.items().map(i => i.name))
  } catch (e) { console.log('EXC', e.stack) }
  const failed = results.filter(r => !r[1]).length
  console.log(`${results.length - failed}/${results.length} passed`)
  process.exit(failed ? 1 : 0)
})
setTimeout(() => { console.log('TIMEOUT'); process.exit(2) }, 90000)
