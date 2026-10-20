// ES module side of real.cjs: imports a `typescript/...` export from the user's real TypeScript.
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const { realModulePath } = require('./real.cjs');

export function importReal(subpath) {
  return import(pathToFileURL(realModulePath(subpath)).href);
}
