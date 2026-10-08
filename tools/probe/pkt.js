const mineflayer = require('mineflayer')
const bot = mineflayer.createBot({
  host: '127.0.0.1', port: 25565, username: 'Probe',
  version: '1.21.11', auth: 'offline', hideErrors: false,
})
const counts = {}
bot._client.on('packet', (data, meta) => {
  if (meta.state !== 'play') return
  counts[meta.name] = (counts[meta.name] || 0) + 1
})
bot.on('error', e => console.log('ERR', e.message))
bot.on('kicked', r => console.log('KICK', JSON.stringify(r).slice(0, 400)))
bot.once('spawn', () => console.log('SPAWNED'))
setTimeout(() => {
  console.log('packets seen in play:')
  for (const [k, v] of Object.entries(counts).sort((a,b)=>b[1]-a[1])) console.log(' ', k, v)
  console.log('bot.entity:', bot.entity ? 'present' : 'MISSING')
  process.exit(0)
}, 20000)
