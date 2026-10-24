// Module hooks for running a playground spec in plain Node: `~utils` and `vitest` map to the
// shims next to this file, and extensionless relative imports try `.ts`/`.js` the way the
// playground's bundler-style resolution does. Specs load through Node's type stripping.

import { registerHooks } from 'node:module'

const shims = {
  '~utils': new URL('./utils.mjs', import.meta.url).href,
  vitest: new URL('./vitest.mjs', import.meta.url).href,
}

registerHooks({
  resolve(specifier, context, next) {
    if (shims[specifier]) return { url: shims[specifier], shortCircuit: true }
    try {
      return next(specifier, context)
    } catch (e) {
      if (!/^\.\.?\//.test(specifier) || e?.code !== 'ERR_MODULE_NOT_FOUND') throw e
      for (const ext of ['.ts', '.js', '/index.ts', '/index.js']) {
        try {
          return next(specifier + ext, context)
        } catch {}
      }
      throw e
    }
  },
  // A playground whose package.json says `"type": "commonjs"` still has ESM specs.
  load(url, context, next) {
    if (/\/__tests__\/[^/]+\.spec\.ts$/.test(url)) return next(url, { ...context, format: 'module-typescript' })
    return next(url, context)
  },
})
