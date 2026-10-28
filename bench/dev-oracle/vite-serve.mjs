// Starts Vite's dev server the way the playground's startDefaultServe does (its config file
// merged under the test defaults: polling watcher, VITE_INLINE=inline-serve) on a fixed
// port, and prints `Local: <url>` once listening. Usage: node vite-serve.mjs <root> <port>
// [config]: a variant spec's `vite.config-<variant>.js` instead of the root's config.

import { createServer, createLogger, loadConfigFromFile, mergeConfig } from 'vite'

const [root, port, configFile] = process.argv.slice(2)
process.env.VITE_INLINE = 'inline-serve'
const res = await loadConfigFromFile({ command: 'serve', mode: 'development' }, configFile, root)
const base = createLogger('info')
const log = (msg) => process.stdout.write(`[server] ${msg}\n`)
const customLogger = { ...base, info: log, warn: log, warnOnce: log, error: log }
let config = mergeConfig(
  {
    root,
    logLevel: 'silent',
    configFile: false,
    server: { watch: { usePolling: true, interval: 100 } },
    build: { target: 'esnext' },
    customLogger,
  },
  res?.config ?? {},
)
config = mergeConfig(config, { server: { port: Number(port), strictPort: true } })
const server = await (await createServer(config)).listen()
process.stdout.write(`Local: ${server.resolvedUrls.local[0]}\n`)
const stop = () => server.close().finally(() => process.exit(0))
process.on('SIGTERM', stop)
process.on('SIGINT', stop)
