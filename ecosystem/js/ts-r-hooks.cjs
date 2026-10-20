// Loader used by the ecosystem tests stage in place of ts-node / tsx / tap's TypeScript
// loader: TypeScript files are compiled by `ts-r-run --compile <file>` (prints "esm" or
// "cjs", a newline, then the code). Installed by replacing those packages' entry files
// in the test copy's node_modules. Needs Node 22.15+ (`module.registerHooks`).
//
// Env: TS_R_ECO_RUN_BIN (ts-r-run), TS_R_ECO_SHIM_LOG (one line per event:
// tool<TAB>exit<TAB>detail).
'use strict';

const { registerHooks } = require('node:module');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const { fileURLToPath } = require('node:url');

const TS = /\.(?:ts|tsx|mts|cts)$/;
const DTS = /\.d\.(?:ts|mts|cts)$/;

function record(tool, code, detail) {
  const log = process.env.TS_R_ECO_SHIM_LOG;
  if (!log) return;
  try {
    fs.appendFileSync(log, `${tool}\t${code}\t${String(detail).replace(/\s+/g, ' ').slice(0, 300)}\n`);
  } catch {}
}

// `./x.js` written for a `./x.ts` source (TypeScript's convention), or an extensionless
// relative import of a TypeScript file.
function tsCandidates(spec) {
  const swaps = [
    [/\.js$/, ['.ts', '.tsx']],
    [/\.mjs$/, ['.mts']],
    [/\.cjs$/, ['.cts']],
    [/\.jsx$/, ['.tsx']],
  ];
  for (const [from, tos] of swaps) {
    if (from.test(spec)) return tos.map((to) => spec.replace(from, to));
  }
  if (/\.[a-z]+$/i.test(spec)) return [];
  return ['.ts', '.tsx', '/index.ts', '/index.tsx'].map((e) => spec + e);
}

if (!globalThis.__tsREcosystemHooks) {
  globalThis.__tsREcosystemHooks = true;
  const bin = process.env.TS_R_ECO_RUN_BIN;
  registerHooks({
    resolve(specifier, context, nextResolve) {
      try {
        return nextResolve(specifier, context);
      } catch (err) {
        const parent = context.parentURL;
        if (parent && parent.startsWith('file:') && /^\.{1,2}\//.test(specifier)) {
          for (const cand of tsCandidates(specifier)) {
            const url = new URL(cand, parent);
            if (fs.existsSync(fileURLToPath(url))) return nextResolve(url.href, context);
          }
        }
        throw err;
      }
    },
    load(url, context, nextLoad) {
      if (!url.startsWith('file:')) return nextLoad(url, context);
      const file = fileURLToPath(url);
      if (!TS.test(file) || DTS.test(file)) return nextLoad(url, context);
      let out;
      try {
        out = execFileSync(bin, ['--compile', file], {
          encoding: 'utf8',
          maxBuffer: 256 * 1024 * 1024,
          stdio: ['ignore', 'pipe', 'pipe'],
        });
      } catch (e) {
        const msg = String(e.stderr || e.message).trim();
        record('ts-r-run', 1, `${file}: ${msg.split('\n')[0]}`);
        throw new Error(`ts-r-run --compile failed for ${file}\n${msg}`);
      }
      const nl = out.indexOf('\n');
      const format = out.slice(0, nl).trim() === 'cjs' ? 'commonjs' : 'module';
      record('ts-r-run', 0, file);
      return { format, source: out.slice(nl + 1), shortCircuit: true };
    },
  });
}

