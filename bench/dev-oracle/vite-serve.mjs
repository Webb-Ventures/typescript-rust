// Starts Vite's dev server the way the playground's startDefaultServe does (its config file
// merged under the test defaults: polling watcher, VITE_INLINE=inline-serve) on a fixed
// port, and prints `Local: <url>` once listening. Usage: node vite-serve.mjs <root> <port>
// [config]: a variant spec's `vite.config-<variant>.js` instead of the root's config.
// Vite is imported from TS_R_BENCH_MODULES (run.mjs's `--modules`), else bench/node_modules.

import fs from 'node:fs'
import path from 'node:path'
import { pathToFileURL } from 'node:url'

const modules = process.env.TS_R_BENCH_MODULES ?? path.resolve(import.meta.dirname, '../node_modules')
const vitePkg = path.join(modules, 'vite')
const exp = JSON.parse(fs.readFileSync(path.join(vitePkg, 'package.json'), 'utf8')).exports?.['.']
const entry = typeof exp === 'string' ? exp : (exp?.import?.default ?? exp?.import ?? exp?.default)
const { createServer, createLogger, loadConfigFromFile, mergeConfig } = await import(
  typeof entry === 'string' ? pathToFileURL(path.join(vitePkg, entry)).href : 'vite'
)

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
