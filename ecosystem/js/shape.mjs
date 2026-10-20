// Loads a module and prints its exported shape as the last line of stdout (JSON): the
// module's own type and the sorted export keys with the typeof of each value. Exits right
// away so timers or open handles in package code don't keep the process alive.
//
// Usage: node shape.mjs <file> <esm|cjs>

import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const [file, format] = process.argv.slice(2);
const done = (o) => {
  process.stdout.write('\n' + JSON.stringify(o) + '\n', () => process.exit(0));
};
const kind = (v) => (v === null ? 'null' : Array.isArray(v) ? 'array' : typeof v);

let m;
let loaded = false;
try {
  m = format === 'esm' ? await import(pathToFileURL(file).href) : createRequire(import.meta.url)(file);
  loaded = true;
} catch (e) {
  const msg = String((e && e.message) || e).split('\n')[0].slice(0, 300);
  done({ error: `${(e && e.name) || 'Error'}: ${msg}` });
}
if (loaded) {
  const keys = m !== null && (typeof m === 'object' || typeof m === 'function') ? Object.keys(m).sort() : [];
  done({
    type: kind(m),
    keys: keys.slice(0, 2000).map((k) => {
      try {
        return [k, kind(m[k])];
      } catch {
        return [k, 'throws'];
      }
    }),
  });
}
