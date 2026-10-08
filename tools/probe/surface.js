// What does the world actually look like now? Read the column under the bot.
const mineflayer = require('mineflayer')
const bot = mineflayer.createBot({ host:'127.0.0.1', port:25565, username:'SurfProbe', version:'1.21.11', auth:'offline' })
const sleep = ms => new Promise(r=>setTimeout(r,ms))
bot.on('error', e=>console.log('ERR', e.message))
bot.once('spawn', async () => {
  await sleep(25000)
  const { Vec3 } = require('vec3')
  const p = bot.entity.position.floored()
  console.log('at', p.toString())
  for (const [dx,dz] of [[0,0],[8,8],[-20,30],[40,-60]]) {
    const names = []
    for (let y = 200; y >= -64; y--) {
      const b = bot.blockAt(new Vec3(p.x+dx, y, p.z+dz))
      if (!b) continue
      if (b.name !== 'air' || names.length) names.push(`${y}:${b.name}`)
      if (names.length > 8) break
    }
    const bottom = []
    for (let y = -64; y <= -58; y++) {
      const b = bot.blockAt(new Vec3(p.x+dx, y, p.z+dz))
      bottom.push(`${y}:${b?b.name:'?'}`)
    }
    console.log(`(${p.x+dx},${p.z+dz}) top: ${names.join(' ')}`)
    console.log(`            bottom: ${bottom.join(' ')}`)
  }
  process.exit(0)
})
setTimeout(()=>{console.log('TIMEOUT');process.exit(1)},90000)
