// How long a real join takes: time to spawn, and how fast columns arrive.
const mineflayer = require('mineflayer')
const t0 = Date.now()
const bot = mineflayer.createBot({
  host: '127.0.0.1', port: 25565, username: 'SpeedProbe',
  version: '1.21.11', auth: 'offline', hideErrors: false,
})
let cols = 0, first = null
bot._client.on('packet', (d, m) => {
  if (m.state === 'play' && m.name === 'map_chunk') {
    cols++
    if (first === null) first = Date.now() - t0
  }
})
bot.on('error', e => console.log('ERR', e.message))
bot.on('kicked', r => console.log('KICK', JSON.stringify(r).slice(0, 300)))
bot.once('spawn', () => console.log(`spawned after ${Date.now() - t0} ms`))
const total = Number(process.argv[2] || 60)
let t = 0
const iv = setInterval(() => {
  t += 5
  console.log(`  ${t}s: ${cols} columns`)
  if (t >= total) {
    console.log(`first column after ${first} ms; final: ${cols} columns in ${total}s`)
    clearInterval(iv); process.exit(0)
  }
}, 5000)
