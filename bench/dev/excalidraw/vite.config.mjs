// Vite config for the excalidraw dev-server benchmark, loaded by both servers: Vite (with
// @vitejs/plugin-react, the reference) and ts-r-dev (which reads plugin-react's options and
// replaces the plugin with its built-in transform). Vite and the plugin come from
// TSR_VITE_TOOLS (bench/, where bench/package.json pins them), so nothing is installed into
// the app. Differences from excalidraw-app/vite.config.mts, the same for both servers:
//   - only plugin-react (no PWA, ESLint checker, EJS, sitemap or woff2 plugins);
//   - .scss imports resolve to an empty stylesheet: ts-r-dev has no CSS preprocessors yet,
//     so neither server compiles Sass and the page renders unstyled.
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
const pkgs = path.resolve(here, '../packages')
const tools = process.env.TSR_VITE_TOOLS
if (!tools) throw new Error('set TSR_VITE_TOOLS to a directory whose node_modules has @vitejs/plugin-react')
const react = (
  await import(pathToFileURL(path.join(tools, 'node_modules/@vitejs/plugin-react/dist/index.js')).href)
).default

// excalidraw-app's aliases: the packages resolve to their sources, not their dist builds.
const src = { common: 'common/src', element: 'element/src', math: 'math/src', utils: 'utils/src' }
const entry = {
  common: 'common/src/index.ts',
  element: 'element/src/index.ts',
  excalidraw: 'excalidraw/index.tsx',
  math: 'math/src/index.ts',
  utils: 'utils/src/index.ts',
  'fractional-indexing': 'fractional-indexing/src/index.ts',
  'laser-pointer': 'laser-pointer/src/index.ts',
}
const alias = [{ find: /^.*\.scss$/, replacement: path.join(here, 'empty.css') }]
for (const [name, file] of Object.entries(entry)) {
  alias.push({ find: new RegExp(`^@excalidraw/${name}$`), replacement: path.join(pkgs, file) })
  const dir = name === 'excalidraw' ? 'excalidraw' : src[name]
  if (dir) alias.push({ find: new RegExp(`^@excalidraw/${name}/(.*?)`), replacement: `${path.join(pkgs, dir)}/$1` })
}

export default {
  plugins: [react()],
  envDir: '..',
  publicDir: '../public',
  cacheDir: '../node_modules/.vite-ts-r-bench',
  resolve: { alias },
  logLevel: 'info',
  clearScreen: false,
  server: { strictPort: true },
}
