// Module hooks for running tsx's spec files (TypeScript, run by Node's own type stripping) with
// the oracle's stand-ins: `manten`, tests/utils/tsx and tests/utils/pty-shell are replaced, tsx's root package.json is
// a stub, test libraries resolve from bench/run-oracle/node_modules, and the suite's relative
// `.js` / extensionless imports of its own `.ts` files are mapped to those files.

import fs from 'node:fs';
import { registerHooks } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = new URL('./', import.meta.url);
const replaced = {
  manten: new URL('manten.mjs', here).href,
};
const harnessDeps = new Set(['execa', 'expect', 'fs-fixture', 'outdent', 'strip-ansi', 'type-fest']);

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (replaced[specifier]) return { url: replaced[specifier], shortCircuit: true };
    if (/(^|\/)utils\/tsx(\.js)?$/.test(specifier)) {
      return { url: new URL('tsx.mjs', here).href, shortCircuit: true };
    }
    if (/(^|\/)utils\/pty-shell(\/index)?(\.js)?$/.test(specifier)) {
      return { url: new URL('pty-shell.mjs', here).href, shortCircuit: true };
    }
    if (specifier === '../../package.json') {
      return { url: new URL('package-json.mjs', here).href, shortCircuit: true };
    }
    if (harnessDeps.has(specifier)) {
      return nextResolve(specifier, { ...context, parentURL: here.href });
    }
    if (specifier.startsWith('.') && context.parentURL?.startsWith('file:')) {
      const target = new URL(specifier, context.parentURL);
      const file = fileURLToPath(target);
      if (!fs.existsSync(file) || fs.statSync(file).isDirectory()) {
        const base = file.replace(/\.js$/, '');
        for (const candidate of [`${base}.ts`, `${base}/index.ts`]) {
          if (fs.existsSync(candidate)) {
            return { url: pathToFileURL(candidate).href, shortCircuit: true };
          }
        }
      }
    }
    return nextResolve(specifier, context);
  },
});
