// Finds the user's real TypeScript and our ts-r-check binary. No dependencies.
'use strict';

const fs = require('fs');
const path = require('path');

const OWN_DIR = path.join(__dirname, '..');
const OWN = require('../package.json');

// Node platform key -> npm platform suffix. Keep in sync with PLATFORMS in npm/ts-r/bin/ts-r.js
// and TARGETS in npm/scripts/build-packages.mjs (the build script checks all three).
const PLATFORMS = {
  'darwin-arm64': 'darwin-arm64',
  'darwin-x64': 'darwin-x64',
  'linux-arm64-gnu': 'linux-arm64-gnu',
  'linux-arm64-musl': 'linux-arm64-musl',
  'linux-x64-gnu': 'linux-x64-gnu',
  'linux-x64-musl': 'linux-x64-musl',
  'win32-arm64': 'win32-arm64-msvc',
  'win32-x64': 'win32-x64-msvc',
};

const EXE = process.platform === 'win32' ? '.exe' : '';

function isMusl() {
  try {
    const report = process.report && process.report.getReport();
    if (report && report.header) return !report.header.glibcVersionRuntime;
  } catch (_) {
    // Fall through to the ldd check.
  }
  try {
    return fs.readFileSync('/usr/bin/ldd', 'utf8').includes('musl');
  } catch (_) {
    return false;
  }
}

function platformKey() {
  const base = `${process.platform}-${process.arch}`;
  return process.platform === 'linux' ? `${base}-${isMusl() ? 'musl' : 'gnu'}` : base;
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (_) {
    return null;
  }
}

/** A real TypeScript package in `dir`, or null (our own package doesn't count). */
function realAt(dir) {
  const pkg = readJson(path.join(dir, 'package.json'));
  if (!pkg || pkg.name !== 'typescript' || pkg['ts-r']) return null;
  const tsc = pkg.bin && (typeof pkg.bin === 'string' ? pkg.bin : pkg.bin.tsc);
  if (!tsc) return null;
  return { dir, version: pkg.version, tsc: path.join(dir, tsc), exports: pkg.exports || {} };
}

let cachedReal;

/**
 * The user's real TypeScript: $TS_R_TYPESCRIPT (a package directory), then `typescript` when
 * that isn't this package (we were installed as @ts-r.dev/typescript next to it), then the
 * `typescript-real` alias (we were installed under the `typescript` name). Each name is resolved
 * from the working directory first, then from this package.
 */
function findRealTypeScript() {
  if (cachedReal !== undefined) return cachedReal;
  cachedReal = null;
  const env = process.env.TS_R_TYPESCRIPT;
  if (env) {
    cachedReal = realAt(path.resolve(env));
    return cachedReal;
  }
  for (const name of ['typescript', 'typescript-real']) {
    for (const from of [process.cwd(), OWN_DIR]) {
      try {
        const json = require.resolve(`${name}/package.json`, { paths: [from] });
        const found = realAt(path.dirname(json));
        if (found) {
          cachedReal = found;
          return found;
        }
      } catch (_) {
        // Not installed under this name.
      }
    }
  }
  return null;
}

/** Path of the real TypeScript's module for an export subpath (`./unstable/sync`). */
function realModulePath(subpath) {
  const real = findRealTypeScript();
  if (!real) {
    throw new Error(
      `@ts-r.dev/typescript: TypeScript's "${subpath}" API comes from your own TypeScript, which wasn't found. ` +
        'Install typescript@7 next to @ts-r.dev/typescript, or add "typescript-real": "npm:typescript@7" when ' +
        '@ts-r.dev/typescript is installed under the typescript name (see its README).',
    );
  }
  let target = real.exports[subpath];
  while (target && typeof target === 'object') target = target.import || target.default || target.require;
  if (typeof target !== 'string') {
    throw new Error(`@ts-r.dev/typescript: TypeScript ${real.version} has no "${subpath}" export.`);
  }
  return path.join(real.dir, target);
}

/** The ts-r-check binary: $TS_R_BIN_DIR, else the @ts-r.dev/check platform package. */
function findTsrCheck() {
  const name = `ts-r-check${EXE}`;
  if (process.env.TS_R_BIN_DIR) {
    const bin = path.join(process.env.TS_R_BIN_DIR, name);
    if (fs.existsSync(bin)) return bin;
  }
  const suffix = PLATFORMS[platformKey()];
  if (!suffix) return null;
  try {
    const frontJson = require.resolve('@ts-r.dev/check/package.json', { paths: [OWN_DIR] });
    const prefix = readJson(frontJson)['ts-r'].platformPrefix;
    const platJson = require.resolve(`${prefix}${suffix}/package.json`, { paths: [path.dirname(frontJson)] });
    const bin = path.join(path.dirname(platJson), 'bin', name);
    return fs.existsSync(bin) ? bin : null;
  } catch (_) {
    return null;
  }
}

module.exports = { OWN, OWN_DIR, PLATFORMS, findRealTypeScript, realModulePath, findTsrCheck };
