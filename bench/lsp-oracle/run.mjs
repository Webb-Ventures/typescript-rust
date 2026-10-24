// LSP oracle for ts-r-lsp.
//
// Reads TypeScript's fourslash tests (tests/cases/fourslash/*.ts at the pinned TypeScript
// commit, fetched by `cargo xtask upstream sync`; used as data only: the files, compiler
// directives, markers and ranges, and which language feature each test verifies), writes each
// test's files to a temporary project, opens them in TypeScript 7.0.2's language server
// (`tsc --lsp --stdio`, bench/node_modules) and in ts-r-lsp (the ts-r backend), sends both the
// same request at every marker, and compares the normalized answers. The expectations written
// in the fourslash file are not used: tsc 7's live answer is the oracle.
//
// Usage (from the repo root, always under limit.sh):
//   scripts/limit.sh --mem-mb 4096 --time-s 3600 -- node bench/lsp-oracle/run.mjs \
//     [--filter REGEX] [--feature hover,completion,...] [--jobs 4] [--max N]
//     [--tsr target/release/ts-r-lsp] [--tsc PATH] [--tests DIR] [--out FILE] [--show N] [--keep]
//   --tsr defaults to $CARGO_TARGET_DIR/release/ts-r-lsp when CARGO_TARGET_DIR is set; --tsc
//   to tsc 7's native binary in bench/node_modules (darwin-arm64); --tests to the fourslash
//   directory; --out to target/lsp-oracle/results.json.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
const opt = (name, def) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : def;
};
const filter = opt('filter') ? new RegExp(opt('filter')) : null;
const onlyFeatures = opt('feature') ? new Set(opt('feature').split(',')) : null;
const jobs = Math.min(4, Number(opt('jobs', '4')));
const max = Number(opt('max', '0'));
const tsr = path.resolve(root, opt('tsr', process.env.CARGO_TARGET_DIR ? path.join(process.env.CARGO_TARGET_DIR, 'release/ts-r-lsp') : 'target/release/ts-r-lsp'));
const tsc = path.resolve(root, opt('tsc', 'bench/node_modules/@typescript/typescript-darwin-arm64/lib/tsc'));
const testsDir = path.resolve(root, opt('tests', 'target/upstream/typescript/tests/cases/fourslash'));
const outFile = path.resolve(root, opt('out', 'target/lsp-oracle/results.json'));
const show = Number(opt('show', '0'));
const keep = args.includes('--keep');
const REQUEST_MS = 20000;
const RESTART_EVERY = 60;

// ---------------------------------------------------------------------------------------------
// Fourslash files as data

/** Fourslash directive name (lower case) -> tsconfig compilerOptions name. */
const OPTIONS = Object.fromEntries(
  [
    'allowImportingTsExtensions', 'allowJs', 'allowSyntheticDefaultImports', 'allowUnreachableCode',
    'allowUnusedLabels', 'baseUrl', 'checkJs', 'customConditions', 'declaration', 'emitDecoratorMetadata',
    'esModuleInterop', 'exactOptionalPropertyTypes', 'experimentalDecorators', 'importHelpers',
    'isolatedDeclarations', 'isolatedModules', 'jsx', 'jsxFactory', 'lib', 'module', 'moduleResolution',
    'noEmit', 'noErrorTruncation', 'noImplicitAny', 'noImplicitOverride', 'noImplicitThis', 'noLib',
    'noPropertyAccessFromIndexSignature', 'noUncheckedSideEffectImports', 'noUnusedLocals',
    'noUnusedParameters', 'outDir', 'resolveJsonModule', 'rootDirs', 'skipLibCheck', 'sourceMap',
    'strict', 'strictNullChecks', 'target', 'typeRoots', 'types', 'useDefineForClassFields',
    'verbatimModuleSyntax', 'newLine', 'sourceRoot', 'mapRoot', 'noImplicitReferences',
    'preserveValueImports', 'importsNotUsedAsValues', 'maxNodeModuleJsDepth',
  ].map((o) => [o.toLowerCase(), o]),
);
const LIST_OPTIONS = new Set(['lib', 'types', 'typeRoots', 'rootDirs', 'customConditions']);
/** Harness directives that don't change what a server answers. */
const IGNORED = new Set(['filename', 'baselinefile', 'emitthisfile', 'stabletypeordering', 'traceresolution', 'noemit', 'sourcemap', 'declaration', 'outdir', 'sourceroot', 'maproot']);
/** Directives that need something this harness can't set up the same way for both servers. */
const UNSUPPORTED = {
  symlink: 'symlinks', link: 'symlinks', outfile: 'outFile', allownontsextensions: 'non-TS extensions',
  resolvereference: 'resolveReference', ts: 'harness directive', noimplicitreferences: 'noImplicitReferences',
};

const FEATURES = [
  ['hover', /verify\.(not\.)?(quickInfo|baselineQuickInfo)/],
  ['completion', /verify\.(not\.)?completions\(/],
  ['definition', /goToDefinition|baselineGoToDefinition/i],
  ['typeDefinition', /goToTypeDefinition|baselineGoToType/],
  ['references', /baselineFindAllReferences|verify\.(singleReferenceGroup|referenceGroups|baselineReferences)/],
  ['rename', /baselineRename|verify\.rename/],
  ['signatureHelp', /verify\.(not\.)?(signatureHelp|baselineSignatureHelp|noSignatureHelp)/],
  ['highlights', /baselineDocumentHighlights|verify\.documentHighlights/],
];
const SOURCE = /\.(d\.)?([cm]?[tj]sx?)$/;

/** Splits one file's text into its content without markers, and the markers and ranges in it. */
function stripMarkers(raw) {
  let out = '';
  const markers = [];
  const ranges = [];
  const open = [];
  let i = 0;
  while (i < raw.length) {
    if (raw.startsWith('/*', i)) {
      const end = raw.indexOf('*/', i + 2);
      const name = end >= 0 ? raw.slice(i + 2, end) : null;
      if (name !== null && /^[\w$.\-]*$/.test(name) && !name.startsWith('*')) {
        markers.push({ name, offset: out.length });
        i = end + 2;
        continue;
      }
    }
    if (raw.startsWith('{|', i)) {
      const end = raw.indexOf('|}', i + 2);
      if (end >= 0) {
        let name = '';
        try {
          name = String(JSON.parse(`{${raw.slice(i + 2, end)}}`).name ?? '');
        } catch {}
        markers.push({ name, offset: out.length });
        i = end + 2;
        continue;
      }
    }
    if (raw.startsWith('[|', i)) {
      open.push(out.length);
      i += 2;
      continue;
    }
    if (raw.startsWith('|]', i) && open.length) {
      ranges.push({ start: open.pop(), end: out.length });
      i += 2;
      continue;
    }
    out += raw[i++];
  }
  return { text: out, markers, ranges };
}

function parseTest(file) {
  const src = fs.readFileSync(file, 'utf8');
  const why = [];
  const options = {};
  const files = [];
  let cur = null;
  let pending = [];
  const lines = src.split(/\r?\n/);
  for (const line of lines) {
    const content = /^\s*\/\/\/\/ ?(.*)$/.exec(line);
    if (content) {
      if (!cur) {
        cur = { name: 'test.ts', lines: [] };
        files.push(cur);
      }
      cur.lines.push(content[1]);
      continue;
    }
    const dir = /^\s*\/\/\s*@(\w+)\s*:\s*(.*?)\s*$/.exec(line);
    if (!dir) continue;
    const key = dir[1].toLowerCase();
    const value = dir[2];
    if (key === 'filename') {
      cur = { name: value.replace(/^\/+/, ''), lines: [] };
      files.push(cur);
      continue;
    }
    if (IGNORED.has(key)) continue;
    if (UNSUPPORTED[key]) {
      why.push(UNSUPPORTED[key]);
      continue;
    }
    const name = OPTIONS[key];
    if (!name) {
      why.push(`directive @${dir[1]}`);
      continue;
    }
    if (!LIST_OPTIONS.has(name) && value.includes(',')) pending.push(name); // variants: the first one
    const first = LIST_OPTIONS.has(name) ? value : value.split(',')[0].trim();
    options[name] =
      first === 'true' ? true : first === 'false' ? false : /^\d+$/.test(first) ? Number(first)
        : LIST_OPTIONS.has(name) ? first.split(',').map((s) => s.trim()).filter(Boolean) : first;
  }
  const out = [];
  for (const f of files) {
    if (f.name.includes('..') || /^[a-z]:/i.test(f.name)) why.push('path outside the project');
    out.push({ name: f.name, ...stripMarkers(f.lines.join('\n')) });
  }
  if (!out.length) why.push('no files');
  if (new Set(out.map((f) => f.name)).size !== out.length) why.push('duplicate file names');
  if (out.some((f) => !SOURCE.test(f.name) && !/\.json$/.test(f.name))) why.push('non-source files');
  const features = FEATURES.filter(([, re]) => re.test(src)).map(([n]) => n);
  if (!features.length) why.push('no compared feature');
  if (/\bedit\.|goTo\.file\(|verify\.(baselineCommands|organizeImports)|format\./.test(src)) {
    // Edits between verifications: the servers see the text before any edit, which is still a
    // valid comparison of the two servers (not of the fourslash expectations).
  }
  if (/\bconfigurePlugin\b|@plugins/.test(src)) why.push('plugins');
  return { files: out, options, features, why: [...new Set(why)], variants: pending };
}

function loadCases() {
  const cases = [];
  for (const name of fs.readdirSync(testsDir).sort()) {
    if (!name.endsWith('.ts') || name === 'fourslash.ts') continue;
    cases.push({ name: name.replace(/\.ts$/, ''), path: path.join(testsDir, name) });
  }
  return cases;
}

// ---------------------------------------------------------------------------------------------
// LSP client

class Lsp {
  constructor(label, cmd, argv, opts) {
    this.label = label;
    this.child = spawn(cmd, argv, { stdio: ['pipe', 'pipe', 'pipe'], ...opts });
    this.buf = Buffer.alloc(0);
    this.seq = 0;
    this.pending = new Map();
    this.exited = false;
    this.stderr = '';
    this.child.stdout.on('data', (d) => this.onData(d));
    this.child.stderr.on('data', (d) => (this.stderr = (this.stderr + d).slice(-4000)));
    this.child.on('exit', () => {
      this.exited = true;
      for (const p of this.pending.values()) p.reject(new Error(`${label} exited: ${this.stderr.slice(-500)}`));
      this.pending.clear();
    });
    this.child.stdin.on('error', () => {});
  }
  onData(d) {
    this.buf = Buffer.concat([this.buf, d]);
    for (;;) {
      const sep = this.buf.indexOf('\r\n\r\n');
      if (sep < 0) return;
      const m = /Content-Length: *(\d+)/i.exec(this.buf.slice(0, sep).toString());
      if (!m) {
        this.buf = this.buf.slice(sep + 4);
        continue;
      }
      const len = Number(m[1]);
      if (this.buf.length < sep + 4 + len) return;
      const msg = JSON.parse(this.buf.slice(sep + 4, sep + 4 + len).toString('utf8'));
      this.buf = this.buf.slice(sep + 4 + len);
      this.onMessage(msg);
    }
  }
  onMessage(msg) {
    if (msg.id !== undefined && msg.method) {
      // A request from the server: configuration gets one null per item, the rest null.
      const result = msg.method === 'workspace/configuration' ? (msg.params?.items ?? []).map(() => null) : null;
      this.write({ jsonrpc: '2.0', id: msg.id, result });
      return;
    }
    if (msg.id !== undefined) {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      clearTimeout(p.timer);
      if (msg.error) p.resolve({ error: msg.error });
      else p.resolve({ result: msg.result });
    }
  }
  write(msg) {
    if (this.exited) return;
    const body = Buffer.from(JSON.stringify(msg), 'utf8');
    this.child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    this.child.stdin.write(body);
  }
  request(method, params, ms = REQUEST_MS) {
    if (this.exited) return Promise.reject(new Error(`${this.label} exited: ${this.stderr.slice(-500)}`));
    const id = ++this.seq;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        this.write({ jsonrpc: '2.0', method: '$/cancelRequest', params: { id } });
        reject(new Error(`${this.label}: ${method} timed out`));
      }, ms);
      this.pending.set(id, { resolve, reject, timer });
      this.write({ jsonrpc: '2.0', id, method, params });
    });
  }
  notify(method, params) {
    this.write({ jsonrpc: '2.0', method, params });
  }
  async stop() {
    if (this.exited) return;
    try {
      await this.request('shutdown', null, 3000);
      this.notify('exit', null);
    } catch {}
    await new Promise((r) => setTimeout(r, 200));
    if (!this.exited) this.child.kill('SIGKILL');
  }
}

const CAPS = {
  general: { positionEncodings: ['utf-16'] },
  textDocument: {
    hover: { contentFormat: ['markdown', 'plaintext'] },
    completion: { completionItem: { snippetSupport: false, labelDetailsSupport: true } },
    definition: { linkSupport: false },
    typeDefinition: { linkSupport: false },
    signatureHelp: { signatureInformation: { parameterInformation: { labelOffsetSupport: true }, activeParameterSupport: true } },
    rename: { prepareSupport: false },
    documentHighlight: {},
    synchronization: { didSave: false },
  },
  workspace: { configuration: true, workspaceFolders: false },
};

/** The two servers of one worker. */
class Pair {
  constructor(dir, index) {
    this.dir = dir;
    this.index = index;
    this.cases = 0;
  }
  async start() {
    const cache = path.join(this.dir, 'cache');
    fs.mkdirSync(cache, { recursive: true });
    const env = {
      ...process.env,
      TS_R_CACHE_DIR: cache,
      TS_R_LSP_TYPE_BACKEND: 'ts-r',
      TS_R_LSP_PARITY: 'off',
      TS_R_LSP_WATCH: 'off',
      TS_R_LSP_IDLE_SHUTDOWN_MS: '300000',
    };
    // The daemon runs in the foreground as our child (same process group as limit.sh's), so
    // nothing outlives the run; the front attaches to it.
    this.daemon = spawn(tsr, ['daemon', '--root', this.dir], { env, stdio: ['ignore', 'ignore', 'pipe'], cwd: this.dir });
    this.daemonLog = '';
    this.daemon.stderr.on('data', (d) => (this.daemonLog = (this.daemonLog + d).slice(-2000)));
    const lspDir = path.join(cache, 'lsp');
    for (let i = 0; i < 200 && !(fs.existsSync(lspDir) && fs.readdirSync(lspDir).some((f) => f.endsWith('.pid'))); i++) {
      await new Promise((r) => setTimeout(r, 25));
    }
    await new Promise((r) => setTimeout(r, 50));
    this.ours = new Lsp('ts-r-lsp', tsr, ['--root', this.dir, '--name', 'lsp-oracle', '--lint', 'off'], { env, cwd: this.dir });
    this.theirs = new Lsp('tsc --lsp', tsc, ['--lsp', '--stdio'], { cwd: this.dir });
    const init = { processId: process.pid, rootUri: pathToFileURL(this.dir).href, capabilities: CAPS, workspaceFolders: null };
    await Promise.all([this.ours.request('initialize', init, 60000), this.theirs.request('initialize', init, 60000)]);
    this.ours.notify('initialized', {});
    this.theirs.notify('initialized', {});
  }
  async stop() {
    await Promise.all([this.ours?.stop(), this.theirs?.stop()]);
    if (this.daemon && this.daemon.exitCode === null) {
      this.daemon.kill('SIGTERM');
      await new Promise((r) => setTimeout(r, 100));
      if (this.daemon.exitCode === null) this.daemon.kill('SIGKILL');
    }
    this.ours = this.theirs = this.daemon = null;
  }
}

// ---------------------------------------------------------------------------------------------
// Requests and normalization

const position = (text, offset) => {
  const before = text.slice(0, offset);
  const nl = before.lastIndexOf('\n');
  return { line: before.split('\n').length - 1, character: offset - nl - 1 };
};

function languageId(name) {
  if (name.endsWith('.json')) return 'json';
  if (/\.[cm]?tsx$/.test(name)) return 'typescriptreact';
  if (/\.[cm]?ts$/.test(name)) return 'typescript';
  if (/\.[cm]?jsx$/.test(name)) return 'javascriptreact';
  return 'javascript';
}

/** A URI as the case-relative path, or `lib:<file>` for TypeScript's lib files. */
function uriKey(uri, caseDir) {
  let p;
  try {
    p = fileURLToPath(uri);
  } catch {
    p = uri;
  }
  const lib = /(lib\.[\w.]*\.d\.ts)$/.exec(p);
  if (lib && !p.startsWith(caseDir)) return `lib:${lib[1]}`;
  const real = (() => {
    try {
      return fs.realpathSync(p);
    } catch {
      return p;
    }
  })();
  return path.relative(caseDir, real).split(path.sep).join('/');
}
const rangeKey = (r) => `${r.start.line}:${r.start.character}-${r.end.line}:${r.end.character}`;
const ws = (s) => s.replace(/\r\n/g, '\n').replace(/[ \t]+/g, ' ').replace(/\n\s*\n+/g, '\n\n').trim();

function hoverText(result) {
  if (!result) return null;
  let c = result.contents;
  const part = (x) => (typeof x === 'string' ? x : x.language ? `\`\`\`${x.language}\n${x.value}\n\`\`\`` : x.value ?? '');
  c = Array.isArray(c) ? c.map(part).join('\n\n') : part(c);
  if (!c.trim()) return null;
  // Code fences: drop the language tag (tsc says `tsx`), keep the code.
  const code = [];
  const prose = c.replace(/```[\w-]*\n([\s\S]*?)\n?```/g, (_, body) => {
    code.push(body);
    return '';
  });
  return { code: code.map(ws).join('\n'), doc: ws(prose) };
}

function normalize(feature, r, caseDir) {
  if (!r || r.error || r.result == null) return null;
  const v = r.result;
  switch (feature) {
    case 'hover':
      return hoverText(v);
    case 'definition':
    case 'typeDefinition':
    case 'references': {
      const list = Array.isArray(v) ? v : [v];
      const keys = list.map((l) => `${uriKey(l.uri ?? l.targetUri, caseDir)}:${rangeKey(l.range ?? l.targetSelectionRange)}`);
      return keys.length ? [...new Set(keys)].sort() : null;
    }
    case 'rename': {
      const keys = [];
      for (const [uri, edits] of Object.entries(v.changes ?? {})) for (const e of edits) keys.push(`${uriKey(uri, caseDir)}:${rangeKey(e.range)}:${e.newText}`);
      for (const dc of v.documentChanges ?? []) for (const e of dc.edits ?? []) keys.push(`${uriKey(dc.textDocument.uri, caseDir)}:${rangeKey(e.range)}:${e.newText}`);
      return keys.length ? [...new Set(keys)].sort() : null;
    }
    case 'highlights': {
      const keys = v.map((h) => `${rangeKey(h.range)}:${h.kind ?? 1}`);
      return keys.length ? [...new Set(keys)].sort() : null;
    }
    case 'completion': {
      const items = Array.isArray(v) ? v : v.items ?? [];
      // An empty list is no list (tsc answers null where ts-r answers `items: []`).
      if (!items.length) return null;
      // Entries tsc ranks above globals and keywords (sortText "10"-"14": locals, members,
      // optional members, ...): the part of the list that is about the code at the marker.
      const near = items.filter((i) => (i.sortText ?? '15') < '15').map((i) => i.label);
      return { near: [...new Set(near)].sort(), all: new Set(items.map((i) => i.label)), size: items.length };
    }
    case 'signatureHelp': {
      if (!v.signatures?.length) return null;
      const active = v.activeSignature ?? 0;
      const sig = v.signatures[active];
      return {
        labels: v.signatures.map((s) => ws(s.label)),
        active,
        param: sig?.activeParameter ?? v.activeParameter ?? 0,
      };
    }
  }
  return v;
}

/** Compares one answer pair: null when they agree, else a short description of the difference. */
function compare(feature, a, b) {
  if (feature === 'completion') {
    if (!a && !b) return null;
    if (!a || !b) return `${a ? 'ts-r' : 'tsc'} gave no list`;
    // Our list must hold tsc's near entries, and our own near entries must be in tsc's list.
    const missing = a.near.filter((l) => !b.all.has(l));
    const extra = b.near.filter((l) => !a.all.has(l));
    if (!missing.length && !extra.length) return null;
    return `missing [${missing.slice(0, 12).join(', ')}]${missing.length > 12 ? ` +${missing.length - 12}` : ''} extra [${extra.slice(0, 12).join(', ')}]${extra.length > 12 ? ` +${extra.length - 12}` : ''}`;
  }
  const sa = JSON.stringify(a);
  const sb = JSON.stringify(b);
  if (sa === sb) return null;
  if (feature === 'hover' && a && b && a.code === b.code) return `docs differ: tsc ${JSON.stringify(a.doc)} ts-r ${JSON.stringify(b.doc)}`;
  return `tsc ${sa}\nts-r ${sb}`;
}

function request(feature, uri, pos) {
  const td = { textDocument: { uri }, position: pos };
  switch (feature) {
    case 'hover':
      return ['textDocument/hover', td];
    case 'completion':
      return ['textDocument/completion', { ...td, context: { triggerKind: 1 } }];
    case 'definition':
      return ['textDocument/definition', td];
    case 'typeDefinition':
      return ['textDocument/typeDefinition', td];
    case 'references':
      return ['textDocument/references', { ...td, context: { includeDeclaration: true } }];
    case 'rename':
      return ['textDocument/rename', { ...td, newName: 'renamedX' }];
    case 'signatureHelp':
      return ['textDocument/signatureHelp', { ...td, context: { triggerKind: 1, isRetrigger: false } }];
    case 'highlights':
      return ['textDocument/documentHighlight', td];
  }
}

// ---------------------------------------------------------------------------------------------
// One case

async function runCase(pair, c, test) {
  const caseDir = path.join(pair.dir, `c${c.id}`);
  fs.mkdirSync(caseDir, { recursive: true });
  const hasConfig = test.files.some((f) => /(^|\/)[tj]sconfig\.json$/.test(f.name));
  for (const f of test.files) {
    const p = path.join(caseDir, f.name);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, f.text);
  }
  if (!hasConfig) {
    fs.writeFileSync(path.join(caseDir, 'tsconfig.json'), JSON.stringify({ compilerOptions: test.options, include: ['**/*'] }, null, 2));
  }
  const opened = test.files.filter((f) => SOURCE.test(f.name) || f.name.endsWith('.json')).filter((f) => !/(^|\/)[tj]sconfig\.json$/.test(f.name));
  const uris = new Map(opened.map((f) => [f.name, pathToFileURL(path.join(caseDir, f.name)).href]));
  for (const s of [pair.theirs, pair.ours]) {
    for (const f of opened) s.notify('textDocument/didOpen', { textDocument: { uri: uris.get(f.name), languageId: languageId(f.name), version: 1, text: f.text } });
  }
  const positions = [];
  for (const f of opened) for (const m of f.markers) positions.push({ file: f, name: m.name || '(anonymous)', offset: m.offset });
  if (!positions.length) for (const f of opened) for (const r of f.ranges) positions.push({ file: f, name: `range@${r.start}`, offset: r.start });
  const results = [];
  let broken = null;
  for (const feature of test.features) {
    if (onlyFeatures && !onlyFeatures.has(feature)) continue;
    if (!positions.length) {
      results.push({ feature, status: 'skip', why: ['no markers'] });
      continue;
    }
    let compared = 0;
    let agree = 0;
    const diffs = [];
    for (const p of positions) {
      const [method, params] = request(feature, uris.get(p.file.name), position(p.file.text, p.offset));
      let ra;
      let rb;
      try {
        [ra, rb] = await Promise.all([
          pair.theirs.request(method, params).catch((e) => ({ error: { message: String(e.message) }, crash: true })),
          pair.ours.request(method, params).catch((e) => ({ error: { message: String(e.message) }, crash: true })),
        ]);
      } catch (e) {
        broken = e;
        break;
      }
      if (ra.crash || rb.crash) broken ??= new Error(ra.crash ? ra.error.message : rb.error.message);
      const a = normalize(feature, ra, caseDir);
      const b = rb.crash ? { error: rb.error.message } : normalize(feature, rb, caseDir);
      if (ra.crash) break;
      if (a == null && b == null) continue;
      compared++;
      const d = compare(feature, a, b);
      if (d === null) agree++;
      else diffs.push(`${p.file.name} /*${p.name}*/: ${d.length > 600 ? `${d.slice(0, 600)}...` : d}`);
      if (broken) break;
    }
    if (broken && /tsc --lsp/.test(String(broken.message))) results.push({ feature, status: 'skip', why: ['tsc did not answer'] });
    else if (!compared) results.push({ feature, status: 'skip', why: ['both answers empty'] });
    else results.push({ feature, status: diffs.length ? 'fail' : 'pass', queries: compared, agree, detail: diffs.slice(0, 6).join('\n') });
    if (broken) break;
  }
  if (!broken) {
    for (const s of [pair.theirs, pair.ours]) for (const f of opened) s.notify('textDocument/didClose', { textDocument: { uri: uris.get(f.name) } });
  }
  return { results, broken };
}

async function main() {
  for (const p of [tsr, tsc, testsDir]) {
    if (!fs.existsSync(p)) {
      console.error(`missing ${p} (build ts-r-lsp --release, npm ci in bench/, cargo xtask upstream sync)`);
      process.exit(2);
    }
  }
  if (!process.env.TS_R_LIB_DIR) console.error('note: TS_R_LIB_DIR is not set; ts-r-lsp looks for lib files next to itself');
  let cases = loadCases();
  if (filter) cases = cases.filter((c) => filter.test(c.name));
  if (max) cases = cases.slice(0, max);
  cases.forEach((c, i) => (c.id = i));
  const tmp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'tsr-lsp-oracle-')));
  const rows = [];
  let next = 0;
  let done = 0;
  async function worker(w) {
    const pair = new Pair(path.join(tmp, `w${w}`), w);
    fs.mkdirSync(pair.dir, { recursive: true });
    let started = false;
    for (;;) {
      const i = next++;
      if (i >= cases.length) break;
      const c = cases[i];
      const test = parseTest(c.path);
      const wanted = onlyFeatures ? test.features.filter((f) => onlyFeatures.has(f)) : test.features;
      if (test.why.length || !wanted.length) {
        for (const f of wanted.length ? wanted : ['-']) rows.push({ name: c.name, feature: f, status: 'skip', why: test.why.length ? test.why : ['no compared feature'] });
      } else {
        if (!started || pair.cases >= RESTART_EVERY) {
          if (started) await pair.stop();
          await pair.start();
          started = true;
          pair.cases = 0;
        }
        pair.cases++;
        let out;
        try {
          out = await runCase(pair, c, test);
        } catch (e) {
          out = { results: wanted.map((f) => ({ feature: f, status: 'fail', detail: `harness: ${e.stack}` })), broken: e };
        }
        for (const r of out.results) rows.push({ name: c.name, variants: test.variants.length ? test.variants : undefined, ...r });
        if (out.broken) {
          await pair.stop();
          started = false;
        }
        if (!keep) fs.rmSync(path.join(pair.dir, `c${c.id}`), { recursive: true, force: true });
      }
      done++;
      if (done % 200 === 0) process.stderr.write(`${done}/${cases.length}\n`);
    }
    if (started) await pair.stop();
  }
  await Promise.all(Array.from({ length: jobs }, (_, w) => worker(w)));
  if (!keep) fs.rmSync(tmp, { recursive: true, force: true });

  rows.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.feature < b.feature ? -1 : 1));
  const perFeature = {};
  const skipReasons = {};
  let queries = 0;
  let agree = 0;
  for (const r of rows) {
    if (r.feature === '-') {
      for (const w of r.why) skipReasons[w] = (skipReasons[w] ?? 0) + 1;
      continue;
    }
    const f = (perFeature[r.feature] ??= { pass: 0, fail: 0, skip: 0, queries: 0, agree: 0 });
    f[r.status]++;
    if (r.status === 'skip') for (const w of r.why) skipReasons[w] = (skipReasons[w] ?? 0) + 1;
    else {
      f.queries += r.queries ?? 0;
      f.agree += r.agree ?? 0;
      queries += r.queries ?? 0;
      agree += r.agree ?? 0;
    }
  }
  const by = (s) => rows.filter((r) => r.status === s && r.feature !== '-').length;
  const summary = {
    typescript: '7.0.2',
    tests: cases.length,
    cases: rows.filter((r) => r.feature !== '-').length,
    compared: by('pass') + by('fail'),
    pass: by('pass'),
    fail: by('fail'),
    skip: by('skip'),
    passRate: by('pass') / Math.max(1, by('pass') + by('fail')),
    queries,
    agree,
    perFeature,
    skipReasons: Object.fromEntries(Object.entries(skipReasons).sort((a, b) => b[1] - a[1])),
  };
  fs.mkdirSync(path.dirname(outFile), { recursive: true });
  fs.writeFileSync(outFile, JSON.stringify({ summary, results: rows }, null, 2));
  for (const r of rows.filter((x) => x.status === 'fail').slice(0, show)) console.log(`\n=== FAIL ${r.name} ${r.feature}\n${r.detail}`);
  console.log(JSON.stringify({ ...summary, perFeature: undefined, skipReasons: undefined }, null, 2));
  for (const [f, v] of Object.entries(perFeature)) console.log(`${f}: ${v.pass} pass, ${v.fail} fail, ${v.skip} skip; queries ${v.agree}/${v.queries} agree`);
  console.log('skips:', JSON.stringify(summary.skipReasons));
}

main();
