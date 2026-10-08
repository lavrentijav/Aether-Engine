// End-to-end survival check against a running aether-server (1.21.11):
// health/time arrive, digging drops an item that is picked up, crafting in
// the inventory grid works, Q throws, a summoned mob can be hit and killed,
// and mobs and items appear as entities.
const mineflayer = require('mineflayer')
const { Vec3 } = require('vec3')
const bot = mineflayer.createBot({ host: '127.0.0.1', port: 25565, username: process.argv[2] || 'Probe', version: '1.21.11', auth: 'offline' })
const sleep = ms => new Promise(r => setTimeout(r, ms))
const results = []
const check = (name, ok, extra = '') => { results.push([name, ok]); console.log(ok ? 'PASS' : 'FAIL', name, extra) }
bot.on('error', e => console.log('ERR', e.message))
bot.on('kicked', r => console.log('KICKED', JSON.stringify(r)))
bot.once('spawn', async () => {
  try {
    await sleep(2500)
    check('health sent', bot.health === 20, `hp=${bot.health} food=${bot.food}`)
    check('time ticks', bot.time.timeOfDay > 0, `tod=${bot.time.timeOfDay}`)
    check('survival mode', bot.game.gameMode === 'survival', bot.game.gameMode)
    const pos = bot.entity.position.floored()
    console.log('at', pos.toString())
    // Dig the block under our feet's neighbour (dirt/grass), wait for pickup.
    let target = null
    for (let r = 1; r < 4 && !target; r++) for (let dx = -r; dx <= r && !target; dx++) for (let dz = -r; dz <= r && !target; dz++) {
      const b = bot.blockAt(pos.offset(dx, -1, dz))
      if (b && ['grass_block', 'dirt', 'sand'].includes(b.name)) target = b
    }
    if (!target) { check('found diggable block', false); } else {
      const name = target.name
      await bot.dig(target, true)
      await sleep(1500)
      const after = bot.blockAt(target.position)
      check('block broken', after && after.name === 'air', after && after.name)
      const want = name === 'grass_block' ? 'dirt' : name
      const have = bot.inventory.items().find(i => i.name === want)
      check('drop picked up', !!have, JSON.stringify(bot.inventory.items().map(i => i.name + 'x' + i.count)))
    }
    // Give logs (operator) and craft planks + table via the 2x2 grid.
    bot.chat('/give oak_log 4')
    await sleep(800)
    check('give works', !!bot.inventory.items().find(i => i.name === 'oak_log'))
    const planksRecipe = bot.recipesFor(bot.registry.itemsByName.oak_planks.id, null, 1, null)[0]
    const countOf = n => bot.inventory.items().filter(i => i.name === n).reduce((a, i) => a + i.count, 0)
    const planksBefore = countOf('oak_planks')
    if (planksRecipe) {
      await bot.craft(planksRecipe, 2, null)
      await sleep(800)
      check('craft planks', countOf('oak_planks') - planksBefore === 8, countOf('oak_planks') - planksBefore)
    } else check('craft planks (recipe)', false)
    const tableRecipe = bot.recipesFor(bot.registry.itemsByName.crafting_table.id, null, 1, null)[0]
    if (tableRecipe) {
      await bot.craft(tableRecipe, 1, null)
      await sleep(800)
      check('craft table', !!bot.inventory.items().find(i => i.name === 'crafting_table'))
    }
    // Throw something with Q and see the item entity.
    const planks = bot.inventory.items().find(i => i.name === 'oak_planks')
    if (planks) {
      await bot.toss(planks.type, null, 1)
      await sleep(600)
      const ent = Object.values(bot.entities).find(e => e.name === 'item')
      check('thrown item entity', !!ent)
    }
    // Summon a zombie and fight it.
    bot.chat('/give diamond_sword 1')
    await sleep(500)
    const sword = bot.inventory.items().find(i => i.name === 'diamond_sword')
    if (sword) await bot.equip(sword, 'hand')
    // Fight on dry land, away from the spawn's lake.
    bot.chat('/tp 27 70 37')
    await sleep(2500)
    bot.chat('/summon zombie')
    await sleep(1500)
    const z = bot.nearestEntity(e => e.name === 'zombie')
    check('zombie visible', !!z)
    if (z) {
      let hits = 0
      const start = Date.now()
      while (bot.entities[z.id] && Date.now() - start < 15000) {
        await bot.lookAt(z.position.offset(0, 1.5, 0), true)
        bot.attack(z)
        hits++
        await sleep(700)
      }
      check('zombie killed', !bot.entities[z.id], `hits=${hits} hp=${bot.health}`)
      await sleep(2500)
      const loot = bot.inventory.items().find(i => i.name === 'rotten_flesh')
      console.log('loot picked:', !!loot, 'xp', bot.experience.points)
    }
    const ents = Object.values(bot.entities).map(e => e.name)
    console.log('entities:', [...new Set(ents)].join(','))
    // Eat: give bread, lose some hunger is hard to force; just check that eating packet is accepted.
    bot.chat('/time set night')
    await sleep(800)
    check('time set', bot.time.timeOfDay >= 13000 && bot.time.timeOfDay < 14000, bot.time.timeOfDay)
    bot.chat('/time set day')
  } catch (e) { console.log('EXC', e.stack) }
  const failed = results.filter(r => !r[1]).length
  console.log(`${results.length - failed}/${results.length} passed`)
  process.exit(failed ? 1 : 0)
})
setTimeout(() => { console.log('TIMEOUT'); process.exit(2) }, 90000)
