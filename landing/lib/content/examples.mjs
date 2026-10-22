// Example blocks on error-code pages, shared by the site (lib/content/docs.mjs) and the example runner.
//
// In content/errors/<CODE>.md:
//   ```ts example                one file of the example (index.ts; tsx/js: index.tsx/index.js)
//   ```ts example file=lib.ts    another file of the same example
//   ```json tsconfig             compilerOptions merged over DEFAULT_TSCONFIG
//   ```ts spec                   a sound-mode spec case from design record 0007 (never run)
//
// `node scripts/verify-examples.mjs` runs every example through ts-r-check and records the
// output in data/error-examples.json under a hash of the blocks. The build shows an example
// only when its recorded run reported the page's code.

import crypto from 'node:crypto';

export const DEFAULT_TSCONFIG = {
  compilerOptions: {
    strict: true,
    target: 'es2022',
    module: 'esnext',
    moduleResolution: 'bundler',
    noEmit: true,
  },
};

const FENCE = /^(`{3,})([^\n]*)\n([\s\S]*?)\n\1[ \t]*$/gm;

// Returns { files: [{lang, file, text}], tsconfig: string|null, spec: [{lang, text}] }.
export function exampleBlocks(body) {
  const files = [];
  const spec = [];
  let tsconfig = null;
  for (const m of body.matchAll(FENCE)) {
    const words = m[2].trim().split(/\s+/);
    const lang = words[0];
    if (words.includes('example')) {
      const f = words.find(w => w.startsWith('file='));
      files.push({ lang, file: f ? f.slice(5) : `index.${lang === 'tsx' ? 'tsx' : lang === 'js' ? 'js' : lang === 'jsx' ? 'jsx' : 'ts'}`, text: m[3] });
    } else if (words.includes('tsconfig')) {
      tsconfig = m[3];
    } else if (words.includes('spec')) {
      spec.push({ lang, text: m[3] });
    }
  }
  return { files, tsconfig, spec };
}

export function exampleHash(code, ex) {
  const h = crypto.createHash('sha256');
  h.update(JSON.stringify({ code, files: ex.files, tsconfig: ex.tsconfig }));
  return h.digest('hex').slice(0, 16);
}

// The tsconfig.json written for an example.
export function effectiveTsconfig(ex) {
  const extra = ex.tsconfig ? JSON.parse(ex.tsconfig) : {};
  const opts = { ...DEFAULT_TSCONFIG.compilerOptions, ...(extra.compilerOptions ?? {}) };
  if (ex.files.some(f => /\.jsx?$/.test(f.file)) && opts.allowJs === undefined) opts.allowJs = true;
  const out = { ...extra, compilerOptions: opts };
  return JSON.stringify(out, null, 2) + '\n';
}
