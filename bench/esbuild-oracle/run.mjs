// esbuild oracle for ts-r-build.
//
// Reads esbuild's end-to-end bundler tests (internal/bundler_tests/*.go at the pinned commit,
// used as data only: file maps, entry points and options), turns each case whose options map
// to CLI flags into a temporary project, builds it with esbuild 0.28.2 (bench/node_modules)
// and with ts-r-build using the same flags, runs every entry output under Node, and compares
// what the two bundles do: build success or failure, stdout, and the thrown error.
//
// Usage (from the repo root, always under limit.sh):
//   scripts/limit.sh --mem-mb 4096 --time-s 1800 -- node bench/esbuild-oracle/run.mjs \
//     [--filter REGEX] [--jobs 4] [--tsr target/release/ts-r-build] [--out FILE] [--show N] [--keep]

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
const opt = (name, def) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : def;
};
const filter = opt('filter') ? new RegExp(opt('filter')) : null;
const jobs = Number(opt('jobs', '4'));
const tsr = path.resolve(root, opt('tsr', 'target/release/ts-r-build'));
const esbuild = path.resolve(root, opt('esbuild', 'bench/node_modules/.bin/esbuild'));
const outFile = path.resolve(root, opt('out', 'target/esbuild-oracle/results.json'));
const show = Number(opt('show', '0'));
const keep = args.includes('--keep'); // leave case directories in the temp dir
const testsDir = path.join(root, 'target/upstream/esbuild/internal/bundler_tests');

// ---------------------------------------------------------------------------------------
// A small parser for the Go composite literals the test cases are written in.

function tokenize(src) {
  const toks = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (/\s/.test(c)) { i++; continue; }
    if (src.startsWith('//', i)) { while (i < src.length && src[i] !== '\n') i++; continue; }
    if (src.startsWith('/*', i)) { i = src.indexOf('*/', i + 2) + 2; continue; }
    if (c === '`') {
      const j = src.indexOf('`', i + 1);
      toks.push({ t: 'str', v: src.slice(i + 1, j) });
      i = j + 1;
      continue;
    }
    if (c === '"') {
      let j = i + 1;
      let s = '';
      while (src[j] !== '"') {
        if (src[j] === '\\') {
          const n = src[j + 1];
          const simple = { n: '\n', t: '\t', r: '\r', '"': '"', '\\': '\\', "'": "'", '0': '\0' };
          if (n in simple) { s += simple[n]; j += 2; continue; }
          if (n === 'x') { s += String.fromCharCode(parseInt(src.slice(j + 2, j + 4), 16)); j += 4; continue; }
          if (n === 'u') { s += String.fromCharCode(parseInt(src.slice(j + 2, j + 6), 16)); j += 6; continue; }
          if (n === 'U') { s += String.fromCodePoint(parseInt(src.slice(j + 2, j + 10), 16)); j += 10; continue; }
          s += n; j += 2; continue;
        }
        s += src[j++];
      }
      toks.push({ t: 'str', v: s });
      i = j + 1;
      continue;
    }
    if (c === "'") {
      const j = src.indexOf("'", i + 2);
      toks.push({ t: 'rune', v: src.slice(i + 1, j) });
      i = j + 1;
      continue;
    }
    const m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(src.slice(i, i + 200));
    if (m) { toks.push({ t: 'id', v: m[0] }); i += m[0].length; continue; }
    const n = /^[0-9][0-9a-fA-Fx_.]*/.exec(src.slice(i, i + 50));
    if (n) { toks.push({ t: 'num', v: n[0] }); i += n[0].length; continue; }
    toks.push({ t: 'p', v: c });
    i++;
  }
  return toks;
}

class Parser {
  constructor(toks, pos) { this.toks = toks; this.pos = pos; }
  peek(o = 0) { return this.toks[this.pos + o]; }
  next() { return this.toks[this.pos++]; }
  is(v, o = 0) { const t = this.peek(o); return t && t.t === 'p' && t.v === v; }
  expect(v) { const t = this.next(); if (!t || t.v !== v) throw new Error(`expected ${v}, got ${t && t.v}`); }

  // value := operand ('+' operand)*
  value() {
    let v = this.operand();
    while (this.is('+') && typeof v === 'string') { this.next(); v += this.operand(); }
    // `compat.A | compat.B`: a set of flags.
    if (this.is('|')) {
      const or = [v];
      while (this.is('|')) { this.next(); or.push(this.operand()); }
      return { or };
    }
    return v;
  }

  operand() {
    const t = this.peek();
    if (t.t === 'str') { this.next(); return t.v; }
    if (t.t === 'num') { this.next(); return Number(t.v); }
    if (t.t === 'rune') { this.next(); return { rune: t.v }; }
    if (this.is('{')) return this.composite(null);
    if (this.is('-') && this.peek(1)?.t === 'num') { this.next(); return -Number(this.next().v); }
    // A type or an identifier path, possibly followed by `{...}` or `(...)`.
    let text = '';
    for (;;) {
      const p = this.peek();
      if (!p) break;
      if (p.t === 'id') { text += p.v; this.next(); continue; }
      if (p.t === 'p' && (p.v === '.' || p.v === '*' || p.v === '&')) { text += p.v; this.next(); continue; }
      if (p.t === 'p' && p.v === '[') {
        // `[]T`, `map[K]V`, `[N]T`
        this.next(); text += '[';
        while (!this.is(']')) text += this.next().v;
        this.next(); text += ']';
        continue;
      }
      break;
    }
    if (this.is('{')) return this.composite(text);
    if (this.is('(')) {
      // Function call: keep the text and the argument tokens.
      let depth = 0;
      const inner = [];
      do {
        const p = this.next();
        if (p.t === 'p' && (p.v === '(' || p.v === '{' || p.v === '[')) depth++;
        if (p.t === 'p' && (p.v === ')' || p.v === '}' || p.v === ']')) depth--;
        inner.push(p);
      } while (depth > 0);
      return { call: text, args: inner.slice(1, -1) };
    }
    if (text === '') throw new Error(`unexpected token ${JSON.stringify(this.peek())}`);
    if (text === 'true') return true;
    if (text === 'false') return false;
    if (text === 'nil') return null;
    return { ident: text };
  }

  composite(type) {
    this.expect('{');
    const items = [];
    while (!this.is('}')) {
      const v = this.value();
      if (this.is(':')) {
        this.next();
        const val = this.value();
        items.push({ key: v, value: val });
      } else {
        items.push({ value: v });
      }
      if (this.is(',')) this.next();
    }
    this.expect('}');
    return { type, items };
  }
}

function asObject(c) {
  const o = {};
  for (const it of c.items) {
    const k = typeof it.key === 'string' ? it.key : it.key && it.key.ident;
    o[k] = it.value;
  }
  return o;
}
const asList = (c) => c.items.map((it) => it.value);

function loadCases() {
  const cases = [];
  for (const f of fs.readdirSync(testsDir).filter((f) => f.endsWith('_test.go')).sort()) {
    const src = fs.readFileSync(path.join(testsDir, f), 'utf8');
    const toks = tokenize(src);
    for (let i = 0; i < toks.length; i++) {
      if (toks[i].t === 'id' && toks[i].v === 'func' && toks[i + 1]?.t === 'id' && toks[i + 1].v.startsWith('Test')) {
        const name = toks[i + 1].v.slice(4);
        // Find `expectBundled...(t, bundled{` inside this function.
        let j = i + 2;
        let depth = 0;
        let started = false;
        for (; j < toks.length; j++) {
          const t = toks[j];
          if (t.t === 'p' && t.v === '{') { depth++; started = true; }
          if (t.t === 'p' && t.v === '}') { depth--; if (started && depth === 0) break; }
          if (t.t === 'id' && t.v.startsWith('expectBundled') && toks[j + 1]?.v === '(') {
            let k = j + 2;
            while (!(toks[k].t === 'id' && toks[k].v === 'bundled' && toks[k + 1]?.v === '{')) k++;
            try {
              const p = new Parser(toks, k + 1);
              const lit = p.composite('bundled');
              const spec = asObject(lit);
              // `files: relocateFiles` (a package-level `var`), a file's text in a local
              // `css := ...`: the variable's value, read from the same test file.
              const lookup = (v) => (v?.ident && /^\w+$/.test(v.ident) ? goVariable(toks, v.ident, i, j) ?? v : v);
              for (const key of ['files', 'entryPaths']) spec[key] = lookup(spec[key]);
              if (spec.files?.items) for (const it of spec.files.items) it.value = lookup(it.value);
              cases.push({ file: f, name, kind: t.v, spec, defines: localDefines(toks, i, j) });
            } catch (e) {
              cases.push({ file: f, name, error: `parse: ${e.message}` });
            }
            break;
          }
        }
      }
    }
  }
  return cases;
}

// The value of Go variable `name`: `name := value` between tokens `from` and `to` (the test
// function), else a package-level `var name = value`. Null when it isn't a literal.
function goVariable(toks, name, from, to) {
  const at = (k) => {
    try { return new Parser(toks, k).value(); } catch { return null; }
  };
  for (let k = from; k < to; k++) {
    if (toks[k].t === 'id' && toks[k].v === name && toks[k + 1]?.v === ':' && toks[k + 2]?.v === '=') return at(k + 3);
  }
  for (let k = 0; k + 3 < toks.length; k++) {
    if (toks[k].v === 'var' && toks[k + 1]?.v === name && toks[k + 2]?.v === '=') return at(k + 3);
  }
  return null;
}

// `defines := config.ProcessDefines([]config.DefineData{...})` between tokens `from` and `to`
// (the test function's own defines, referred to as `Defines: &defines`).
function localDefines(toks, from, to) {
  for (let k = from; k < to; k++) {
    if (toks[k].v === 'defines' && toks[k + 1]?.v === ':' && toks[k + 2]?.v === '=' && toks[k + 3]?.v === 'config'
      && toks[k + 5]?.v === 'ProcessDefines' && toks[k + 6]?.v === '(') {
      try { return new Parser(toks, k + 7).value(); } catch { return null; }
    }
  }
  return null;
}

// One `config.DefineData` as `--define:key=value`, or null when the CLI can't express it.
function defineFlag(key, data) {
  const d = asObject(data);
  if (key === null) key = d.KeyParts ? asList(d.KeyParts).join('.') : null;
  // `Flags: config.CallCanBeUnwrappedIfUnused` alone: the CLI's `--pure:name`.
  if (key && d.Flags?.ident === 'config.CallCanBeUnwrappedIfUnused' && Object.keys(d).every((k) => k === 'KeyParts' || k === 'Flags')) return `--pure:${key}`;
  if (Object.keys(d).some((k) => k !== 'KeyParts' && k !== 'DefineExpr')) return null;
  const e = d.DefineExpr ? asObject(d.DefineExpr) : null;
  if (!key || !e) return null;
  let value = null;
  if (e.Parts && Object.keys(e).length === 1) value = asList(e.Parts).join('.');
  else if (e.Constant && Object.keys(e).length === 1) {
    const c = e.Constant;
    const shared = { 'js_ast.ENullShared': 'null', 'js_ast.EUndefinedShared': 'undefined', 'js_ast.ETrueShared': 'true', 'js_ast.EFalseShared': 'false' };
    if (c.ident && shared[c.ident]) value = shared[c.ident];
    else if (c.type === '&js_ast.ENumber') value = String(asObject(c).Value);
    else if (c.type === '&js_ast.EBoolean') value = String(asObject(c).Value === true);
    else if (c.type === '&js_ast.ENull') value = 'null';
    else if (c.type === '&js_ast.EUndefined') value = 'undefined';
    else if (c.type === '&js_ast.EString') {
      const v = asObject(c).Value;
      if (v?.call === 'helpers.StringToUTF16' && v.args.length === 1 && v.args[0].t === 'str') value = JSON.stringify(v.args[0].v);
    }
  }
  return value === null ? null : `--define:${key}=${value}`;
}

// ---------------------------------------------------------------------------------------
// Options -> esbuild CLI flags (also accepted by ts-r-build).

const IGNORED_OPTIONS = new Set(['OmitRuntimeForTests', 'NeedsMetafile', 'ASCIIOnly', 'LegalComments', 'ProfilerNames', 'LineLimit', 'SourceMap', 'AbsOutputBase']);

// `ClassPrivateStaticField` -> `class-private-static-field` (esbuild's CLI feature names).
const kebab = (s) => s.replace(/[A-Z]/g, (c, i) => (i ? '-' : '') + c.toLowerCase());

function flagsFor(c) {
  const s = c.spec;
  const why = [];
  for (const k of Object.keys(s)) {
    if (!['files', 'entryPaths', 'entryPathsAdvanced', 'options', 'expectedScanLog', 'expectedCompileLog', 'debugLogs', 'absWorkingDir'].includes(k)) why.push(`bundled.${k}`);
  }
  if (c.kind !== 'expectBundled' && c.kind !== 'expectBundledUnix') why.push(c.kind);
  const o = s.options ? asObject(s.options) : {};
  const flags = [];
  const out = {};
  let format = null;
  let bundle = false;
  for (const [k, v] of Object.entries(o)) {
    if (IGNORED_OPTIONS.has(k)) continue;
    switch (k) {
      case 'Mode':
        if (v.ident === 'config.ModeBundle') { flags.push('--bundle'); bundle = true; }
        else if (v.ident === 'config.ModePassThrough' || v.ident === 'config.ModeConvertFormat') {}
        else why.push(`Mode=${v.ident}`);
        break;
      case 'AbsOutputFile': out.file = v; break;
      case 'AbsOutputDir': out.dir = v; break;
      case 'OutputFormat': {
        const m = { 'config.FormatESModule': 'esm', 'config.FormatCommonJS': 'cjs', 'config.FormatIIFE': 'iife' }[v.ident];
        if (!m) why.push(`OutputFormat=${v.ident}`); else { format = m; flags.push(`--format=${m}`); }
        break;
      }
      case 'Platform': {
        const m = { 'config.PlatformNode': 'node', 'config.PlatformNeutral': 'neutral', 'config.PlatformBrowser': 'browser' }[v.ident];
        if (!m) why.push(`Platform=${v.ident}`); else flags.push(`--platform=${m}`);
        break;
      }
      case 'CodeSplitting': if (v === true) flags.push('--splitting'); break;
      case 'MinifySyntax': if (v === true) flags.push('--minify-syntax'); break;
      case 'MinifyIdentifiers': if (v === true) flags.push('--minify-identifiers'); break;
      case 'MinifyWhitespace': if (v === true) flags.push('--minify-whitespace'); break;
      case 'KeepNames': if (v === true) flags.push('--keep-names'); break;
      // `regexp.MustCompile("_$")`: Go's pattern syntax, which both CLIs take as written.
      case 'MangleProps':
      case 'ReserveProps': {
        if (v?.call !== 'regexp.MustCompile' || v.args.length !== 1 || v.args[0].t !== 'str') { why.push(k); break; }
        flags.push(`--${k === 'MangleProps' ? 'mangle' : 'reserve'}-props=${v.args[0].v}`);
        break;
      }
      case 'MangleQuoted': flags.push(`--mangle-quoted=${v === true}`); break;
      case 'TreeShaking': flags.push(`--tree-shaking=${v === true}`); break;
      case 'IgnoreDCEAnnotations': if (v === true) flags.push('--ignore-annotations'); break;
      case 'ExternalPackages': if (v === true) flags.push('--packages=external'); break;
      case 'GlobalName': flags.push(`--global-name=${asList(v).join('.')}`); break;
      case 'MainFields': flags.push(`--main-fields=${asList(v).join(',')}`); break;
      case 'Conditions': flags.push(`--conditions=${asList(v).join(',')}`); break;
      case 'ExtensionOrder': flags.push(`--resolve-extensions=${asList(v).join(',')}`); break;
      case 'PublicPath': flags.push(`--public-path=${v}`); break;
      case 'ExtensionToLoader':
        for (const it of v.items) {
          const l = it.value.ident?.replace('config.Loader', '').toLowerCase();
          const map = { js: 'js', jsx: 'jsx', ts: 'ts', tsx: 'tsx', json: 'json', text: 'text', file: 'file', css: 'css', empty: 'empty', base64: 'base64', dataurl: 'dataurl', binary: 'binary', copy: 'copy', default: null, localcss: 'local-css', globalcss: 'global-css', none: null };
          if (!map[l]) { why.push(`Loader=${l}`); continue; }
          flags.push(`--loader:${it.key}=${map[l]}`);
        }
        break;
      case 'ExternalSettings': {
        const es = asObject(v);
        for (const [phase, m] of Object.entries(es)) {
          const mm = asObject(m);
          // Paths matched after resolution: the CLI's `--external:<absolute path>`.
          if (phase === 'PostResolve') {
            if (Object.keys(mm).some((x) => x !== 'Exact')) { why.push(`External.${phase}`); continue; }
            for (const it of mm.Exact.items) flags.push(`--external:\0IN${it.key}`);
            continue;
          }
          if (phase !== 'PreResolve') { why.push(`External.${phase}`); continue; }
          for (const [kind, val] of Object.entries(mm)) {
            if (kind === 'Exact') for (const it of val.items) flags.push(`--external:${it.key}`);
            else if (kind === 'Patterns') {
              for (const it of val.items) {
                const p = asObject(it.value);
                flags.push(`--external:${p.Prefix ?? ''}*${p.Suffix ?? ''}`);
              }
            } else why.push(`External.${kind}`);
          }
        }
        break;
      }
      // `es(2019)`: everything newer than ES2019 is unsupported, which is `--target=es2019`.
      // `compat.ClassField | compat.Arrow`: those features, `--supported:class-field=false`.
      case 'UnsupportedJSFeatures': {
        if (v.call === 'es' && v.args.length === 1 && v.args[0].t === 'num') {
          if (!flags.includes(`--target=es${v.args[0].v}`)) flags.push(`--target=es${v.args[0].v}`);
          break;
        }
        const set = v.or ?? [v];
        if (!set.every((f) => f.ident?.startsWith('compat.'))) { why.push(k); break; }
        for (const f of set) flags.push(`--supported:${kebab(f.ident.slice(7))}=false`);
        break;
      }
      // The `--target` the options came from (it also makes esbuild ignore tsconfig's).
      // A browser name (`chrome10`) only names the target in messages: the test spells out
      // its features in `Unsupported*Features`.
      case 'OriginalTargetEnv': {
        const t = `--target=${String(v).toLowerCase()}`;
        if (/^--target=[a-z]+\d[\d.]*$/.test(t) && !/^--target=es\d/.test(t) && (o.UnsupportedCSSFeatures || o.UnsupportedJSFeatures)) break;
        if (!/^--target=(esnext|es\d+)$/.test(t)) { why.push(k); break; }
        if (!flags.includes(t)) flags.push(t);
        break;
      }
      // CSS features with a CLI name ts-r-build implements.
      case 'UnsupportedCSSFeatures': {
        const set = v.or ?? [v];
        const names = { 'compat.Nesting': 'nesting', 'compat.IsPseudoClass': 'is-pseudo-class', 'compat.InlineStyle': 'inline-style' };
        if (!set.every((f) => names[f.ident])) { why.push(k); break; }
        for (const f of set) flags.push(`--supported:${names[f.ident]}=false`);
        break;
      }
      // `TS: config.TSOptions{Config: config.TSConfig{UseDefineForClassFields: config.True}}`:
      // the same setting in a tsconfig.json at the root of the case (runCase writes it).
      case 'TS': {
        const cfg = asObject(v).Config ? asObject(asObject(v).Config) : null;
        const ok = cfg && Object.keys(asObject(v)).length === 1 && Object.keys(cfg).length > 0
          && Object.entries(cfg).every(([ck, cv]) => ck === 'UseDefineForClassFields' && ['config.True', 'config.False'].includes(cv?.ident));
        if (!ok) { why.push(k); break; }
        out.tsconfig = { compilerOptions: { useDefineForClassFields: cfg.UseDefineForClassFields.ident === 'config.True' } };
        break;
      }
      // Node's NODE_PATH, which both CLIs read.
      case 'AbsNodePaths': out.nodePaths = asList(v); break;
      // `<stdin>` contents resolved from a directory: piped to the CLI, run from there.
      // ts-r-build doesn't read entry code from stdin, so these stay skipped.
      case 'Stdin': {
        const si = asObject(v);
        if (Object.keys(si).some((x) => x !== 'Contents' && x !== 'AbsResolveDir') || typeof si.Contents !== 'string') { why.push(k); break; }
        out.stdin = { contents: si.Contents, dir: si.AbsResolveDir ?? '/' };
        why.push('Stdin (ts-r-build reads no code from stdin)');
        break;
      }
      case 'WriteToStdout': if (v === true) out.stdout = true; break;
      case 'JSBanner': flags.push(`--banner:js=${v}`); break;
      case 'JSX': {
        for (const [jk, jv] of Object.entries(asObject(v))) {
          const parts = jv && jv.type === 'config.DefineExpr' ? asObject(jv).Parts : null;
          const dotted = parts && parts.items ? asList(parts).join('.') : null;
          if (jk === 'Parse') {} // .jsx entries are parsed as JSX anyway
          else if (jk === 'Preserve' && jv === true) flags.push('--jsx=preserve');
          else if (jk === 'AutomaticRuntime' && jv === true) flags.push('--jsx=automatic');
          else if (jk === 'Development' && jv === true) flags.push('--jsx-dev');
          else if (jk === 'Factory' && dotted) flags.push(`--jsx-factory=${dotted}`);
          else if (jk === 'Fragment' && dotted) flags.push(`--jsx-fragment=${dotted}`);
          else if (jk === 'SideEffects' && jv === true) flags.push('--jsx-side-effects');
          // A constant fragment (`&js_ast.EString{...}`): esbuild's CLI takes it as JSON.
          else if (jk === 'Fragment' && jv?.type === 'config.DefineExpr' && Object.keys(asObject(jv)).length === 1
            && asObject(jv).Constant?.type === '&js_ast.EString') {
            const sv = asObject(asObject(jv).Constant).Value;
            if (sv?.call === 'helpers.StringToUTF16' && sv.args.length === 1 && sv.args[0].t === 'str') flags.push(`--jsx-fragment=${JSON.stringify(sv.args[0].v)}`);
            else why.push(`JSX.${jk}`);
          }
          else why.push(`JSX.${jk}`);
        }
        break;
      }
      case 'EntryPathTemplate':
      case 'ChunkPathTemplate':
      case 'AssetPathTemplate': {
        const names = { DirPlaceholder: '[dir]', NamePlaceholder: '[name]', HashPlaceholder: '[hash]', ExtPlaceholder: '[ext]' };
        let t = '';
        for (const part of asList(v)) {
          const po = asObject(part);
          t += po.Data ?? '';
          if (po.Placeholder) {
            const n = names[po.Placeholder.ident?.replace('config.', '')];
            if (!n) { why.push(`${k}.${po.Placeholder.ident}`); break; }
            t += n;
          }
        }
        flags.push(`--${{ EntryPathTemplate: 'entry', ChunkPathTemplate: 'chunk', AssetPathTemplate: 'asset' }[k]}-names=${t}`);
        break;
      }
      case 'OutputExtensionJS': flags.push(`--out-extension:.js=${v}`); break;
      // `Defines: &defines` (the function's `config.ProcessDefines(...)` list), or an inline
      // `&config.ProcessedDefines{IdentifierDefines: ...}`.
      case 'Defines': {
        let list = null;
        if (v?.ident === '&defines' && c.defines?.items) list = c.defines.items.map((it) => [null, it.value]);
        else if (v?.type === '&config.ProcessedDefines') {
          const pd = asObject(v);
          // DotDefines are keyed by the last part, each entry naming its own `KeyParts`.
          if (Object.keys(pd).every((x) => x === 'IdentifierDefines' || x === 'DotDefines')) {
            list = (pd.IdentifierDefines?.items ?? []).map((it) => [it.key, it.value]);
            for (const it of pd.DotDefines?.items ?? []) list.push(...asList(it.value).map((d) => [null, d]));
          }
        }
        const fl = list?.map(([key, d]) => defineFlag(key, d));
        if (!fl || fl.some((x) => x === null)) { why.push(k); break; }
        flags.push(...fl);
        break;
      }
      // The input root is only known per tool run: buildAndRun fills it in.
      case 'TSConfigPath': flags.push(`--tsconfig=\0IN${v}`); break;
      case 'InjectPaths': for (const f of asList(v)) flags.push(`--inject:\0IN${f}`); break;
      case 'DropLabels': flags.push(`--drop-labels=${asList(v).join(',')}`); break;
      // Absolute replacements are paths in the test's file system.
      case 'PackageAliases':
        for (const it of v.items) flags.push(`--alias:${it.key}=${it.value.startsWith?.('/') ? `\0IN${it.value}` : it.value}`);
        break;
      default: why.push(k);
    }
  }
  // No output path: the bundle goes to stdout (one entry point), compared as an output.
  if (!out.file && !out.dir) out.stdout = true;
  if (!s.entryPaths && !s.entryPathsAdvanced && !out.stdin) why.push('no entryPaths');
  if (out.stdout && ((s.entryPaths?.items?.length ?? 0) + (s.entryPathsAdvanced?.items?.length ?? 0) > 1)) why.push('several entry points to stdout');
  return { flags, out, format, bundle, why };
}

// ---------------------------------------------------------------------------------------

function run(cmd, argv, cwd, timeoutMs, { stdin = null, env = {} } = {}) {
  return new Promise((resolve) => {
    const child = spawn(cmd, argv, { cwd, stdio: [stdin === null ? 'ignore' : 'pipe', 'pipe', 'pipe'], env: { ...process.env, TS_R_THREADS: '1', NODE_OPTIONS: '', ...env } });
    if (stdin !== null) child.stdin.end(stdin);
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => { if (stdout.length < 1e6) stdout += d; });
    child.stderr.on('data', (d) => { if (stderr.length < 1e5) stderr += d; });
    const timer = setTimeout(() => { child.kill('SIGKILL'); }, timeoutMs);
    child.on('close', (code, signal) => { clearTimeout(timer); resolve({ code, signal, stdout, stderr }); });
    child.on('error', (e) => { clearTimeout(timer); resolve({ code: -1, stdout: '', stderr: String(e) }); });
  });
}

// Content hashes in file names are implementation-defined.
// A `[hash]` alone as a file name (`--entry-names=[ext]/[hash]`) too.
const normalizeHashes = (s) => s.replace(/-[A-Z2-7]{8}\b/g, '-HASH').replace(/(^|\/)[A-Z2-7]{8}(?=\.\w+\b)/g, '$1HASH');

// CSS has no run-time behaviour under Node, so CSS outputs are compared as text, normalized
// so that only formatting differs: comments dropped, quotes unified, whitespace collapsed and
// removed around punctuation, the last `;` of a block dropped.
function normalizeCss(css) {
  return normalizeHashes(css)
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/'/g, '"')
    .replace(/url\(\s*([^"\s)][^)\s]*)\s*\)/g, 'url("$1")')
    .replace(/\s+/g, ' ')
    .replace(/\s*([{};,>~+])\s*/g, '$1')
    .replace(/:\s+/g, ':')
    .replace(/;+}/g, '}')
    .trim();
}

function errorLine(stderr, dirs) {
  let line = stderr.split('\n').find((l) => /^\w*(Error|Exception)\b|^Error \[/.test(l) || /^Uncaught/.test(l)) ?? '';
  for (const d of dirs) line = line.split(d).join('<dir>');
  // Node's "imported from <file>" names the importing output, which differs by tool.
  // Generated names differ between bundlers (`fn` vs `fn2`): drop numeric suffixes.
  return normalizeHashes(line.replace(/ imported from .*$/, '').replace(/'<dir>[^']*'/g, "'<path>'"))
    .replace(/\b([A-Za-z_$][A-Za-z_$]*?)\d+\b/g, '$1')
    .trim();
}

// `--mangle-props`: the property names matching the pattern that an output still has (after
// `.`, before `:`, quoted, or a class or object member: `name(`, `name =`, `name;` after `{`,
// `}`, `,`, `;` or `static`/`get`/`set`/`async`), less the variables it declares. Output with
// no run-time behaviour would otherwise compare equal however much was mangled. Skipped when
// the pattern matches the short names the mangler produces (`.`, `x`).
function mangleRegex(flags) {
  const f = flags.find((x) => x.startsWith('--mangle-props='));
  if (!f) return null;
  let re;
  try { re = new RegExp(f.slice('--mangle-props='.length)); } catch { return null; }
  return [...'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ'].some((ch) => re.test(ch)) ? null : re;
}
function unmangled(code, re) {
  const set = new Set();
  const text = code.replace(/\/\*[\s\S]*?\*\//g, ' ').replace(/^\s*\/\/.*$/gm, ' ');
  const id = '[A-Za-z_$][\\w$]*';
  const sites = new RegExp([
    `\\.\\s*(${id})`,
    `(${id})(?=\\s*:)`,
    `(["'\`])([^"'\`\\n]*)\\3`,
    `(?:[{},;]|\\b(?:static|get|set|async)\\b|\\*)\\s*(${id})(?=\\s*[(=;])`,
  ].join('|'), 'g');
  for (const m of text.matchAll(sites)) {
    for (const w of (m[1] ?? m[2] ?? m[4] ?? m[5] ?? '').match(/[A-Za-z_$][\w$]*/g) ?? []) if (re.test(w)) set.add(w);
  }
  for (const m of text.matchAll(/\b(?:var|let|const|function|class)\s+([A-Za-z_$][\w$]*)|,\s*([A-Za-z_$][\w$]*)\s*=(?!=)/g)) set.delete(m[1] ?? m[2]);
  return [...set].sort().join(',');
}

async function buildAndRun(tool, bin, c, plan, caseDir) {
  const inRoot = path.join(caseDir, 'in');
  const outRoot = path.join(caseDir, tool);
  const cwd = path.join(inRoot, plan.out.stdin?.dir ?? c.spec.absWorkingDir ?? '/');
  const abs = (e) => (e.startsWith('/') ? path.join(inRoot, e) : e);
  // `entryPathsAdvanced` with an `OutputPath` is the CLI's `out=in` entry syntax.
  const advanced = c.spec.entryPathsAdvanced?.items ? asList(c.spec.entryPathsAdvanced).map(asObject) : [];
  const entries = c.spec.entryPaths?.items ? asList(c.spec.entryPaths).map(abs) : advanced.map((e) => abs(e.InputPath));
  const entryArgs = c.spec.entryPaths?.items ? entries : advanced.map((e, i) => (e.OutputPath ? `${e.OutputPath}=${entries[i]}` : entries[i]));
  const stdoutFile = path.join(outRoot, 'stdout.js');
  const outFlag = plan.out.stdout ? [] : [plan.out.file ? `--outfile=${path.join(outRoot, plan.out.file)}` : `--outdir=${path.join(outRoot, plan.out.dir)}`];
  const meta = path.join(caseDir, `${tool}-meta.json`);
  const extra = tool === 'tsr' ? ['--no-cache'] : [];
  const flags = plan.flags.map((f) => (f.includes('\0IN') ? f.replace(/\0IN(.*)$/, (_, p) => path.join(inRoot, p)) : f));
  const env = plan.out.nodePaths ? { NODE_PATH: plan.out.nodePaths.map((p) => path.join(inRoot, p)).join(path.delimiter) } : {};
  const b = await run(bin, [...entryArgs, ...flags, ...outFlag, ...(plan.out.stdout ? [] : [`--metafile=${meta}`]), '--log-level=error', ...extra], cwd, 30000,
    { stdin: plan.out.stdin?.contents ?? null, env });
  if (b.code !== 0) return { built: false, log: (b.stderr || '').slice(0, 2000), signal: b.signal };
  // Entry outputs, in entry order.
  let outputs = [];
  let css = [];
  if (plan.out.stdout) {
    fs.mkdirSync(outRoot, { recursive: true });
    fs.writeFileSync(stdoutFile, b.stdout);
    outputs = [stdoutFile];
  } else try {
    const m = JSON.parse(fs.readFileSync(meta, 'utf8'));
    const byEntry = new Map();
    for (const [o, info] of Object.entries(m.outputs)) {
      if (info.entryPoint && /\.(c|m)?js$/.test(o)) byEntry.set(path.resolve(cwd, info.entryPoint), path.resolve(cwd, o));
    }
    outputs = entries.map((e) => byEntry.get(path.resolve(cwd, e))).filter(Boolean);
    css = Object.keys(m.outputs).filter((o) => o.endsWith('.css')).map((o) => path.resolve(cwd, o)).sort();
  } catch {
    if (plan.out.file && fs.existsSync(path.join(outRoot, plan.out.file))) outputs = [path.join(outRoot, plan.out.file)];
  }
  const runs = [];
  const mangled = mangleRegex(plan.flags);
  for (const o of outputs) {
    const r = await run(process.execPath, [...(plan.random ? ['--require', plan.random] : []), o], path.dirname(o), 10000);
    runs.push({
      file: path.relative(outRoot, o),
      // Printing `process` (UseStrictDirectiveBundleIssue1837) shows the process ids.
      stdout: normalizeHashes(r.stdout.split(outRoot).join('<out>').split(inRoot).join('<in>'))
        .replace(/^(\s+p?pid): \d+,$/gm, '$1: <pid>,'),
      error: r.code === 0 ? '' : errorLine(r.stderr, [outRoot, inRoot]) || `exit ${r.code}${r.signal ? ` ${r.signal}` : ''}`,
      ...(mangled ? { unmangled: unmangled(fs.readFileSync(o, 'utf8'), mangled) } : {}),
    });
  }
  for (const o of css) {
    let text = '';
    try { text = fs.readFileSync(o, 'utf8'); } catch { text = '<missing>'; }
    runs.push({ file: path.relative(outRoot, o), stdout: normalizeCss(text), error: '' });
  }
  return { built: true, runs };
}

async function runCase(c, tmp) {
  if (c.error) return { status: 'skip', why: [c.error] };
  if (!c.spec.files?.items || !(c.spec.entryPaths?.items || c.spec.entryPathsAdvanced?.items || c.spec.options)) return { status: 'skip', why: ['shared Go variable'] };
  const plan = flagsFor(c);
  if (plan.why.length) return { status: 'skip', why: plan.why };
  const files = asObject(c.spec.files);
  if (Object.values(files).some((v) => typeof v !== 'string')) return { status: 'skip', why: ['non-string file'] };
  const caseDir = path.join(tmp, c.name);
  fs.rmSync(caseDir, { recursive: true, force: true });
  if (plan.out.tsconfig) {
    if (files['/tsconfig.json'] !== undefined) return { status: 'skip', why: ['TS options and a tsconfig.json'] };
    files['/tsconfig.json'] = JSON.stringify(plan.out.tsconfig);
  }
  for (const [p, content] of Object.entries(files)) {
    const full = path.join(caseDir, 'in', p);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, content);
  }
  // `Math.random()` gets the same seeded sequence in both runs.
  if (Object.values(files).some((v) => v.includes('Math.random'))) {
    plan.random = path.join(caseDir, 'random.cjs');
    fs.writeFileSync(plan.random, 'let s = 42;\nMath.random = () => (s = (s * 16807) % 2147483647) / 2147483647;\n');
  }
  fs.mkdirSync(path.join(caseDir, 'in', c.spec.absWorkingDir ?? '/'), { recursive: true });
  const ref = await buildAndRun('esbuild', esbuild, c, plan, caseDir);
  const ours = await buildAndRun('tsr', tsr, c, plan, caseDir);
  if (keep) console.log(`kept ${caseDir}`); else fs.rmSync(caseDir, { recursive: true, force: true });
  let status;
  let detail = '';
  if (!ref.built) {
    status = ours.built ? 'fail' : 'pass';
    detail = ours.built ? `esbuild failed to build:\n${ref.log}` : '';
  } else if (!ours.built) {
    status = 'fail';
    detail = `ts-r-build failed:\n${ours.log}`;
  } else {
    const key = (r) => (r.file.endsWith('.css') ? [normalizeHashes(r.file), r.stdout] : [r.stdout, r.error, ...(r.unmangled !== undefined ? [r.unmangled] : [])]);
    const a = JSON.stringify(ref.runs.map(key));
    const b = JSON.stringify(ours.runs.map(key));
    status = a === b ? 'pass' : 'fail';
    if (status === 'fail') detail = `esbuild: ${a}\nts-r:    ${b}`;
  }
  return { status, detail, flags: plan.flags, refBuilt: ref.built, ranOutput: ref.built && ref.runs.some((r) => r.stdout || r.error) };
}

async function main() {
  for (const p of [tsr, esbuild, testsDir]) {
    if (!fs.existsSync(p)) {
      console.error(`missing ${p} (build ts-r-build --release, npm ci in bench/, cargo xtask upstream sync)`);
      process.exit(2);
    }
  }
  let cases = loadCases();
  if (filter) cases = cases.filter((c) => filter.test(`${c.file}:${c.name}`));
  const tmp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'tsr-esbuild-oracle-')));
  const results = new Array(cases.length);
  let next = 0;
  let done = 0;
  async function worker() {
    for (;;) {
      const i = next++;
      if (i >= cases.length) return;
      try {
        results[i] = { file: cases[i].file, name: cases[i].name, ...(await runCase(cases[i], tmp)) };
      } catch (e) {
        results[i] = { file: cases[i].file, name: cases[i].name, status: 'fail', detail: `harness: ${e.stack}` };
      }
      done++;
      if (done % 100 === 0) process.stderr.write(`${done}/${cases.length}\n`);
    }
  }
  await Promise.all(Array.from({ length: jobs }, worker));
  if (!keep) fs.rmSync(tmp, { recursive: true, force: true });

  const by = (s) => results.filter((r) => r.status === s);
  const pass = by('pass');
  const fail = by('fail');
  const skip = by('skip');
  const perFile = {};
  for (const r of results) {
    const f = (perFile[r.file] ??= { pass: 0, fail: 0, skip: 0 });
    f[r.status]++;
  }
  const skipReasons = {};
  for (const r of skip) for (const w of r.why) skipReasons[w] = (skipReasons[w] ?? 0) + 1;
  const summary = {
    esbuild: '0.28.2',
    commit: '609683d892977362a0f99026cb74b96263d728a9',
    total: results.length,
    run: pass.length + fail.length,
    pass: pass.length,
    fail: fail.length,
    skip: skip.length,
    passRate: pass.length / Math.max(1, pass.length + fail.length),
    withRuntimeOutput: results.filter((r) => r.ranOutput).length,
    perFile,
    skipReasons: Object.fromEntries(Object.entries(skipReasons).sort((a, b) => b[1] - a[1])),
  };
  fs.mkdirSync(path.dirname(outFile), { recursive: true });
  fs.writeFileSync(outFile, JSON.stringify({ summary, results }, null, 2));
  for (const r of fail.slice(0, show)) console.log(`\n=== FAIL ${r.file}:${r.name} ${(r.flags ?? []).join(' ')}\n${r.detail}`);
  console.log(JSON.stringify({ ...summary, skipReasons: undefined, perFile: undefined }, null, 2));
  console.log(Object.entries(perFile).map(([f, v]) => `${f}: ${v.pass} pass, ${v.fail} fail, ${v.skip} skip`).join('\n'));
}

main();
