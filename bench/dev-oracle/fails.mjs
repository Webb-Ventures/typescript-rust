// Lists the dev oracle's ts-r-dev failures from a results file, one block per playground,
// with the first lines of each reason.
//
// node bench/dev-oracle/fails.mjs [results.json] [--playground name] [--skips] [--lines n]

import fs from 'node:fs'
import path from 'node:path'

const here = import.meta.dirname
const args = process.argv.slice(2)
const opt = (name, def) => {
  const i = args.indexOf(name)
  if (i < 0) return def
  const [v] = args.splice(i, 2).slice(1)
  return v
}
const only = opt('--playground')
const lines = Number(opt('--lines', 2))
const skips = args.includes('--skips') && args.splice(args.indexOf('--skips'), 1)
const file = args[0] ?? path.resolve(here, '../../target/dev-oracle/results.json')
const { results } = JSON.parse(fs.readFileSync(file, 'utf8'))

// Paths and ports differ per run; drop them so equal reasons read the same.
const clean = (s) =>
  String(s)
    .split('\n')
    .filter((l) => !/^\s+at /.test(l))
    .slice(0, lines)
    .join('\n      ')
    .replace(/file:\/\/\S+\/work\/(vite|ts-r)\//g, '')
    .replace(/127\.0\.0\.1:\d+/g, 'HOST')

let total = 0
for (const [name, r] of Object.entries(results).sort(([a], [b]) => a.localeCompare(b))) {
  if (only && name !== only) continue
  const out = []
  for (const [spec, server] of Object.entries(r.errors ?? {}).flatMap(([s, e]) => Object.entries(e).map(([k, v]) => [s, [k, v]]))) {
    out.push(`  ${spec} (${server[0]}): ${clean(server[1])}`)
  }
  for (const [spec, tests] of Object.entries(r.specs ?? {})) {
    for (const t of tests) {
      if (t.result !== 'fail' && !(skips && t.result === 'skip')) continue
      total += t.result === 'fail'
      out.push(`  ${t.result === 'skip' ? 'skip ' : ''}${spec} > ${t.id}\n      ${clean(t.reason ?? '')}`)
    }
  }
  if (out.length) console.log(`${name}\n${out.join('\n')}\n`)
}
console.log(`${total} failing tests`)
