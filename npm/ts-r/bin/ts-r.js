#!/usr/bin/env node
// The npm bin shim of every ts-r package. This file is the `@ts-r.dev/cli` package's bin and is copied
// unchanged into each front package (`@ts-r.dev/run`, `@ts-r.dev/check`, ...) as bin/<binary>.js. What
// it runs comes from the "ts-r" key of the package's own package.json:
//
//   { "binary": "ts-r-lint", "platformPrefix": "@ts-r.dev/lint-", "tools": [...], "version": false }
//
// It finds the native binary for this OS, CPU and libc in the matching optional dependency
// (`@ts-r.dev/lint-darwin-arm64`, ...; npm installs only the one whose os/cpu/libc match), runs it
// with the same arguments and inherited stdio, and exits with its exit code (or its signal).
// No dependencies, no install scripts; Node >= 18.
//
// "tools" (the launcher package, `@ts-r.dev/cli`, only) lists the front packages whose binaries the
// launcher should find: their platform packages' bin directories go first on PATH.
// "version": false means the binary has no --version flag, so `--version` is answered here.
//
// Overrides: TS_R_BIN_DIR (a directory holding the binaries, e.g. target/release) wins over
// everything; TS_R_BINARY (a path) replaces the launcher binary in `@ts-r.dev/cli`.
'use strict';

const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const pkgDir = path.join(__dirname, '..');
const pkg = require(path.join(pkgDir, 'package.json'));
// The source tree's npm/ts-r/package.json has no "ts-r" key (build-packages.mjs adds it): run as
// the launcher there, so `TS_R_BIN_DIR=target/release node npm/ts-r/bin/ts-r.js` works.
const cfg = pkg['ts-r'] || { binary: 'ts-r', platformPrefix: '@ts-r.dev/cli-', tools: [] };

// Node platform key -> npm platform suffix. Keep in sync with TARGETS in
// npm/scripts/build-packages.mjs (it checks this table).
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
  // glibcVersionRuntime is only reported when Node runs on glibc.
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
  if (process.platform !== 'linux') return base;
  return `${base}-${isMusl() ? 'musl' : 'gnu'}`;
}

// Mirrors ts_r_cache::cache_root() so binaries installed into the global cache are found.
function cacheRoot() {
  const env = process.env;
  if (env.TS_R_CACHE_DIR) return env.TS_R_CACHE_DIR;
  if (env.XDG_CACHE_HOME && path.isAbsolute(env.XDG_CACHE_HOME)) {
    return path.join(env.XDG_CACHE_HOME, 'ts-r');
  }
  if (process.platform === 'win32' && env.LOCALAPPDATA) return path.join(env.LOCALAPPDATA, 'ts-r');
  return path.join(os.homedir(), '.cache', 'ts-r');
}

// The bin directory of `prefix + platform` resolved from `fromDir`, or null.
function platformBinDir(prefix, fromDir, key) {
  const suffix = PLATFORMS[key];
  if (!suffix) return null;
  try {
    const json = require.resolve(`${prefix}${suffix}/package.json`, { paths: [fromDir] });
    return path.join(path.dirname(json), 'bin');
  } catch (_) {
    return null;
  }
}

function findBinary(key) {
  const name = cfg.binary + EXE;
  const tried = [];
  if (process.env.TS_R_BIN_DIR) {
    const bin = path.join(process.env.TS_R_BIN_DIR, name);
    if (fs.existsSync(bin)) return { bin, tried };
    tried.push(`${bin} (TS_R_BIN_DIR)`);
  }
  if (cfg.binary === 'ts-r' && process.env.TS_R_BINARY) {
    return { bin: process.env.TS_R_BINARY, tried };
  }
  const dir = platformBinDir(cfg.platformPrefix, pkgDir, key);
  if (dir) {
    const bin = path.join(dir, name);
    if (fs.existsSync(bin)) return { bin, tried };
    tried.push(bin);
  } else if (PLATFORMS[key]) {
    tried.push(`the ${cfg.platformPrefix}${PLATFORMS[key]} package (not installed)`);
  }
  const cached = path.join(cacheRoot(), 'bin', pkg.version, name);
  if (fs.existsSync(cached)) return { bin: cached, tried };
  tried.push(cached);
  return { bin: null, tried };
}

function fail(key, tried) {
  const lines = [`${cfg.binary}: couldn't find the ${cfg.binary} ${pkg.version} binary for ${key}.`, '  looked for:'];
  for (const t of tried) lines.push(`    ${t}`);
  if (PLATFORMS[key]) {
    lines.push(
      '',
      `  The optional dependency ${cfg.platformPrefix}${PLATFORMS[key]} should have been installed with ${pkg.name}.`,
      '  - If you installed with --no-optional or --omit=optional, reinstall without it.',
      '  - If node_modules was copied from another machine, reinstall on this one.',
    );
  } else {
    lines.push('', `  ${key} has no prebuilt binary. Supported: ${Object.keys(PLATFORMS).join(', ')}.`);
  }
  lines.push('  - Or set TS_R_BIN_DIR to a directory with the binaries (cargo build --release).');
  process.stderr.write(lines.join('\n') + '\n');
  process.exit(1);
}

// PATH for the launcher: the bin dirs of every installed tool, so `ts-r lint` finds ts-r-lint.
function toolPath(key) {
  const dirs = [];
  for (const front of cfg.tools || []) {
    try {
      const frontJson = require.resolve(`${front}/package.json`, { paths: [pkgDir] });
      const frontPkg = JSON.parse(fs.readFileSync(frontJson, 'utf8'));
      const dir = platformBinDir(frontPkg['ts-r'].platformPrefix, path.dirname(frontJson), key);
      if (dir && fs.existsSync(dir)) dirs.push(dir);
    } catch (_) {
      // Not installed: the launcher reports the missing tool itself.
    }
  }
  if (process.env.TS_R_BIN_DIR) dirs.unshift(process.env.TS_R_BIN_DIR);
  return dirs;
}

function main() {
  const args = process.argv.slice(2);
  if (cfg.version === false && args.length === 1 && (args[0] === '--version' || args[0] === '-v')) {
    process.stdout.write(`${cfg.binary} ${pkg.version}\n`);
    return;
  }
  const key = platformKey();
  const { bin, tried } = findBinary(key);
  if (!bin) fail(key, tried);

  const env = { ...process.env };
  if (cfg.tools) {
    const dirs = toolPath(key);
    const pathKey = Object.keys(env).find((k) => k.toUpperCase() === 'PATH') || 'PATH';
    if (dirs.length) env[pathKey] = dirs.concat(env[pathKey] ? [env[pathKey]] : []).join(path.delimiter);
  }
  const result = spawnSync(bin, args, { stdio: 'inherit', windowsHide: true, env });
  if (result.error) {
    process.stderr.write(`${cfg.binary}: failed to run ${bin}: ${result.error.message}\n`);
    process.exit(1);
  }
  if (result.signal) {
    // Die the same way the binary did, so callers see the signal.
    process.kill(process.pid, result.signal);
    return;
  }
  process.exit(result.status === null ? 1 : result.status);
}

main();
