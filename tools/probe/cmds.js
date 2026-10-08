// Do slash commands reach the server, and does the client get a command tree?
const mineflayer = require('mineflayer')
const bot = mineflayer.createBot({
  host: '127.0.0.1', port: 25565, username: '_alpha_01',
  version: '1.21.11', auth: 'offline', hideErrors: false,
})
bot.on('error', e => console.log('ERR', e.message))
bot.on('kicked', r => console.log('KICK', JSON.stringify(r).slice(0, 300)))
const sleep = ms => new Promise(r => setTimeout(r, ms))

let tree = null
bot._client.on('declare_commands', d => { tree = d })
bot.on('messagestr', m => console.log('   <<', m))

bot.once('spawn', async () => {
  await sleep(1200)
  if (!tree) console.log('NO COMMAND TREE RECEIVED')
  else {
    const names = tree.nodes.filter(n => n.extraNodeData && n.extraNodeData.name)
      .map(n => n.extraNodeData.name)
    console.log(`command tree: ${tree.nodes.length} nodes, root=${tree.rootIndex}`)
    console.log('literals:', names.join(' '))
  }
  for (const c of ['balance', 'inspect', 'audit', 'listings', 'econ give _alpha_01 100 test', 'balance']) {
    console.log('>>', '/' + c)
    bot._client.write('chat_command', { command: c })
    await sleep(600)
  }
  process.exit(0)
})
setTimeout(() => { console.log('TIMEOUT'); process.exit(1) }, 60000)
