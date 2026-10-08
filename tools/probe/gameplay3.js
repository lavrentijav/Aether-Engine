// Third pass: eating, bows, and two players seeing each other's hits.
const mineflayer = require('mineflayer')
const sleep = ms => new Promise(r => setTimeout(r, ms))
const results = []
const check = (name, ok, extra = '') => { results.push([name, ok]); console.log(ok ? 'PASS' : 'FAIL', name, extra) }
const mk = name => mineflayer.createBot({ host: '127.0.0.1', port: 25565, username: name, version: '1.21.11', auth: 'offline' })
const a = mk('Probe'), b = mk('Probe2')
let ready = 0
const go = async () => {
  if (++ready < 2) return
  try {
    await sleep(2000)
    a.chat('/tp 27 70 37'); b.chat('/tp 29 70 37'); await sleep(3000)
    // Eating.
    a.chat('/give bread 2'); a.chat('/food 10'); await sleep(800)
    await a.equip(a.inventory.items().find(i => i.name === 'bread'), 'hand')
    const f0 = a.food
    await a.consume().catch(e => console.log('consume', e.message))
    await sleep(600)
    check('eating restores food', a.food > f0, `${f0} -> ${a.food}`)
    // PvP: a hits b with a sword.
    a.chat('/give iron_sword 1'); await sleep(600)
    await a.equip(a.inventory.items().find(i => i.name === 'iron_sword'), 'hand')
    const pb = a.players.Probe2 && a.players.Probe2.entity
    check('other player visible', !!pb)
    if (pb) {
      const h0 = b.health
      await a.lookAt(pb.position.offset(0, 1.6, 0), true)
      a.attack(pb)
      await sleep(800)
      check('pvp damage', b.health < h0, `${h0} -> ${b.health}`)
    }
    // Bow at a pig.
    a.chat('/give bow 1'); a.chat('/give arrow 8'); a.chat('/summon pig'); await sleep(1500)
    const pig = a.nearestEntity(e => e.name === 'pig')
    await a.equip(a.inventory.items().find(i => i.name === 'bow'), 'hand')
    const arrows0 = a.inventory.items().filter(i => i.name === 'arrow').reduce((s, i) => s + i.count, 0)
    if (pig) await a.lookAt(pig.position.offset(0, 0.5, 0), true)
    a.activateItem(); await sleep(1200); a.deactivateItem(); await sleep(800)
    const arrows1 = a.inventory.items().filter(i => i.name === 'arrow').reduce((s, i) => s + i.count, 0)
    check('bow uses an arrow', arrows1 === arrows0 - 1, `${arrows0} -> ${arrows1}`)
    const arrowEnt = Object.values(b.entities).some(e => e.name === 'arrow')
    console.log('arrow entity seen by other player:', arrowEnt)
  } catch (e) { console.log('EXC', e.stack) }
  const failed = results.filter(r => !r[1]).length
  console.log(`${results.length - failed}/${results.length} passed`)
  process.exit(failed ? 1 : 0)
}
a.once('spawn', go); b.once('spawn', go)
setTimeout(() => { console.log('TIMEOUT'); process.exit(2) }, 60000)
