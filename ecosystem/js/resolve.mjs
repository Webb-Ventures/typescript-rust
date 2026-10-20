// Resolves package specifiers from an install directory the way Node does, for both
// `require` and `import`, without loading any package code.
//
// Usage: node --experimental-import-meta-resolve resolve.mjs <install-dir> <specifier>...
// Prints a JSON array of { spec, require, import } (absolute paths or null).

import { createRequire, isBuiltin } from 'node:module';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const [dir, ...specs] = process.argv.slice(2);
const req = createRequire(path.join(dir, 'noop.js'));
const parent = pathToFileURL(path.join(dir, 'noop.mjs')).href;
const out = [];
for (const spec of specs) {
  let r = null;
  let i = null;
  // Packages named like a built-in (`punycode`, `string_decoder`, `process`) only resolve
  // to the package with a trailing slash.
  const target = isBuiltin(spec) ? `${spec}/` : spec;
  try {
    r = req.resolve(target);
  } catch {}
  if (r === null && target !== spec) {
    try {
      r = req.resolve(path.join(dir, 'node_modules', spec));
    } catch {}
  }
  try {
    const u = import.meta.resolve(target, parent);
    if (u.startsWith('file:')) i = fileURLToPath(u);
  } catch {}
  out.push({ spec, require: r, import: i });
}
process.stdout.write(JSON.stringify(out) + '\n');
