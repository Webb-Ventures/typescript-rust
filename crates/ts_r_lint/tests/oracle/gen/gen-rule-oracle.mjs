// Builds the per-rule oracle files in ../rules/*.json.
//
// Inputs are DATA ONLY: the published rule test files of ESLint and typescript-eslint (their
// `valid`/`invalid` cases) and the observed output of the real ESLint `Linter` on those cases.
// Rule implementations are never loaded: every `require`/`import` of a rule module is replaced
// by an empty stub, and the RuleTester is replaced by a recorder.
//
// Usage (run under scripts/limit.sh):
//   node gen-rule-oracle.mjs --tools <dir with node_modules: eslint, typescript-eslint, globals, typescript>
//        --eslint-tests <eslint>/tests/lib/rules --tseslint-tests <ts-eslint>/packages/eslint-plugin/tests/rules
//        [--fixtures <copy of ts-eslint/packages/eslint-plugin/tests/fixtures>]
//        [--jsdoc-tests <eslint-plugin-jsdoc>/test/rules/assertions]
//        [--unicorn-tests <eslint-plugin-unicorn>/test]
//        --out <crates/ts_r_lint/tests/oracle/rules> [rule ...]
// Pinned: eslint 10.12.0, typescript-eslint 8.71.1, typescript 6.0.3 (private-app's).
//
// Type-aware rules (`meta.docs.requiresTypeChecking`) run with types when `--fixtures` is
// given: each case is linted as a file of that directory (default `file.ts`, `react.tsx`
// with JSX) through the case's `project` or `projectService`, exactly as the typed
// RuleTester does. Keep the copy outside any directory with a `node_modules` above it (no
// `@types` are found, on both sides: tests/oracle/typed mirrors it). The case records
// `typed: { root, project }` and its `filename`, relative to the fixtures directory.
// Rule names: `no-debugger` (ESLint core tests), `@typescript-eslint/no-explicit-any`, or
// `jsdoc/require-returns` (eslint-plugin-jsdoc 63.3.3, from the tools dir: its assertion
// files export `{ valid, invalid }` and its runner adds `ecmaVersion: 'latest'`), or
// `unicorn/no-null` (eslint-plugin-unicorn 79.0.0, from the tools dir with `outdent`: its
// `test/utils/test.js` tester is replaced by a recorder; cases run with its default
// globals, builtin + node + browser, which are written once per file as `globals` and left
// out of each case's own; Vue, HTML, Svelte and non-JavaScript language cases are skipped).

import { registerHooks, createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
const opt = (name) => {
  const i = args.indexOf(name);
  if (i < 0) return undefined;
  const v = args[i + 1];
  args.splice(i, 2);
  return v;
};
const toolsDir = path.resolve(opt('--tools'));
const eslintTests = opt('--eslint-tests');
const tsTests = opt('--tseslint-tests');
const outDir = path.resolve(opt('--out'));
const fixturesOpt = opt('--fixtures');
const jsdocTests = opt('--jsdoc-tests');
const unicornTestsOpt = opt('--unicorn-tests');
const unicornTests = unicornTestsOpt ? fs.realpathSync(path.resolve(unicornTestsOpt)) : undefined;
const fixturesDir = fixturesOpt ? fs.realpathSync(path.resolve(fixturesOpt)) : undefined;
const rules = args.flatMap((a) =>
  a.startsWith('@') && !a.startsWith('@typescript-eslint/')
    ? fs.readFileSync(a.slice(1), 'utf8').split('\n').filter((l) => !l.startsWith('#')).join(' ').split(/\s+/).filter(Boolean)
    : [a],
);

const toolsRequire = createRequire(path.join(toolsDir, 'package.json'));
const { Linter } = toolsRequire('eslint');
const tsParser = toolsRequire('@typescript-eslint/parser');
const tsPlugin = toolsRequire('@typescript-eslint/eslint-plugin');
const jsdocPlugin = jsdocTests ? (await import(pathToFileURL(toolsRequire.resolve('eslint-plugin-jsdoc')).href)).default : undefined;
const unicornPlugin = unicornTests ? (await import(pathToFileURL(toolsRequire.resolve('eslint-plugin-unicorn')).href)).default : undefined;
const unicornGlobals = unicornTests
  ? (() => {
      const g = toolsRequire('globals');
      return { ...g.builtin, ...g.node, ...g.browser };
    })()
  : undefined;

// ---- stubs -------------------------------------------------------------------------------
const stubDir = fs.mkdtempSync(path.join(os.tmpdir(), 'tsr-lint-stubs-'));
const stub = (name, src) => {
  const p = path.join(stubDir, name + '.cjs');
  fs.writeFileSync(p, src);
  return pathToFileURL(p).href;
};
const RULE_TESTER = stub(
  'rule-tester',
  `class RuleTester {
     constructor(config) { this.config = config || {}; }
     run(name, _rule, tests) { globalThis.__recorded.push({ name, config: this.config, tests, defined: [...(this.defined || [])] }); }
     defineRule(name) { (this.defined ||= []).push(name); }
     static describe(_n, f) { f(); } static it(_n, f) { f(); } static itOnly(_n, f) { f(); }
     static only(t) { return t; } static afterAll() {}
   }
   exports.RuleTester = RuleTester;
   exports.noFormat = (raw, ...keys) => String.raw({ raw }, ...keys);
   module.exports = Object.assign(RuleTester, exports);`,
);
const RULE = stub(
  'rule',
  `module.exports = { meta: {}, create() { return {}; } };
   // Names some tests import from the rule module for expected data (never read here).
   module.exports.phrases = new Proxy({}, { get: () => '' });`,
);
// eslint-plugin-unicorn's tester (`test/utils/test.js`) and parser table: the recorder
// lives in `globalThis.__unicornTester` below.
const UNICORN_TEST = stub(
  'unicorn-test',
  `const same = (o) => o;
   const other = (name) => ({ name, __parser: 'other', mergeParserOptions: same });
   exports.parsers = { typescript: { name: 'typescript', __parser: 'ts', mergeParserOptions: same }, vue: other('vue'), html: other('html'), svelte: other('svelte') };
   exports.avoidTestTitleConflict = same;
   exports.normalizeTestCase = (t) => (typeof t === 'string' ? { code: t } : t);
   exports.languages = new Proxy({}, { get: (_t, k) => ({ __language: String(k) }) });
   exports.getTester = (importMeta, ruleId) => globalThis.__unicornTester(importMeta, ruleId);`,
);
const UNICORN_PARSERS = stub(
  'unicorn-parsers',
  `exports.typescriptEslintParser = { __parser: 'ts' };
   exports.vueEslintParser = { __parser: 'other' };
   exports.htmlEslintParser = { __parser: 'other' };
   exports.svelteEslintParser = { __parser: 'other' };`,
);
const ESLINT_NAMED = stub(
  'eslint-named',
  `const real = require(${JSON.stringify(toolsRequire.resolve('eslint'))});
   module.exports = real; exports.Linter = real.Linter; exports.RuleTester = real.RuleTester; exports.SourceCode = real.SourceCode; exports.ESLint = real.ESLint;`,
);
const ESPREE_NAMED = stub(
  'espree-named',
  `const real = require(${JSON.stringify(toolsRequire.resolve('espree'))});
   module.exports = real; exports.parse = real.parse; exports.tokenize = real.tokenize; exports.latestEcmaVersion = real.latestEcmaVersion; exports.version = real.version;`,
);
const NODE_TEST = stub(
  'node-test',
  `const noop = () => {};
   const t = Object.assign(noop, { describe: noop, it: noop, test: noop, only: noop, skip: noop, todo: noop, snapshot: { setDefaultSnapshotSerializers: noop, setResolveSnapshotPath: noop } });
   module.exports = t; exports.describe = noop; exports.it = noop; exports.test = noop; exports.snapshot = t.snapshot;`,
);
// Test-suite helpers that only validate option schemas.
const OPTIONS_VALID = stub('options-valid', `exports.areOptionsValid = () => true;`);
// @typescript-eslint/utils with its enums as plain named exports, so ESM tests can import
// them from the CommonJS build.
const TS_UTILS = stub(
  'ts-utils',
  `const real = require(${JSON.stringify(toolsRequire.resolve('@typescript-eslint/utils'))});
   module.exports = real;
   exports.AST_NODE_TYPES = real.AST_NODE_TYPES;
   exports.AST_TOKEN_TYPES = real.AST_TOKEN_TYPES;
   exports.TSESLint = real.TSESLint;
   exports.ESLintUtils = real.ESLintUtils;`,
);
const UTILS = stub(
  'utils',
  `function unIndent(strings, ...values) {
     const text = strings.map((s, i) => i === 0 ? s : values[i - 1] + s).join('');
     const lines = text.replace(/^\\n/u, '').replace(/\\n\\s*$/u, '').split('\\n');
     const ind = lines.filter(l => l.trim()).map(l => /^\\s*/u.exec(l)[0].length);
     const min = ind.length ? Math.min(...ind) : 0;
     return lines.map(l => l.slice(min)).join('\\n') + '\\n';
   }
   exports.unIndent = unIndent;`,
);
const FIXTURE_PARSER = stub(
  'fixture-parser',
  `function p() { return __filename; } p.__parser = 'fixture'; module.exports = p;`,
);
const TS_PARSER = stub('ts-parser', `module.exports = { __parser: 'ts' };`);
const TS_ESLINT = stub('ts-eslint', `exports.parser = { __parser: 'ts' };`);
const OTHER_PARSER = stub('other-parser', `exports.parseForESLint = () => {};`);
const TS_HELPER = stub(
  'ts-helper',
  `const root = ${JSON.stringify(fixturesDir ?? stubDir)};
   exports.getFixturesRootDir = () => root;
   exports.createRuleTesterWithTypes = (provided = {}) => {
     const parserOptions = { ...provided, tsconfigRootDir: provided.tsconfigRootDir ?? root };
     parserOptions.projectService ??= !parserOptions.project;
     return new (require(${JSON.stringify(new URL(RULE_TESTER).pathname)}).RuleTester)({ languageOptions: { parserOptions } });
   };`,
);
const TS_UTIL = stub('ts-util', `exports.collectVariables = () => ({ unusedVariables: new Set(), usedVariables: new Set() });`);
const VITEST = stub(
  'vitest',
  `exports.describe = globalThis.describe; exports.it = globalThis.it; exports.test = globalThis.it;
   exports.expect = () => new Proxy({}, { get: () => () => {} }); exports.vi = {};`,
);

registerHooks({
  resolve(specifier, context, next) {
    const s = specifier;
    const r = (url) => ({ url, shortCircuit: true, format: 'commonjs' });
    if (/lib\/rule-tester\/rule-tester$/.test(s) || s === '@typescript-eslint/rule-tester') return r(RULE_TESTER);
    if (s.startsWith('.') && /(^|\/)(lib|src)\/rules\//.test(s)) return r(RULE);
    if (s.startsWith('.') && /(^|\/)src\/util(\/|$)/.test(s)) return r(TS_UTIL);
    if (/_utils(\/index(\.js)?)?$/.test(s)) return r(UTILS);
    if (/fixture-parser$/.test(s) || /fixtures\/parsers\//.test(s)) return r(FIXTURE_PARSER);
    if (s === '@typescript-eslint/parser') return r(TS_PARSER);
    if (s === 'typescript-eslint') return r(TS_ESLINT);
    if (s === '@babel/eslint-parser' || s.startsWith('@es-joy/jsdoc-eslint-parser')) return r(OTHER_PARSER);
    if (/(^|\/)RuleTester(\.ts)?$/.test(s)) return r(TS_HELPER);
    if (s === 'vitest') return r(VITEST);
    if (/(^|\/)areOptionsValid(\.ts)?$/.test(s)) return r(OPTIONS_VALID);
    if (s === '@typescript-eslint/utils') return r(TS_UTILS);
    const parent = context.parentURL ?? '';
    if (unicornTests && parent.startsWith(pathToFileURL(unicornTests).href)) {
      if (/(^|\/)utils\/test\.js$/.test(s)) return r(UNICORN_TEST);
      if (/scripts\/parsers\.js$/.test(s)) return r(UNICORN_PARSERS);
      if (s === 'node:test') return r(NODE_TEST);
      if (s === 'eslint') return r(ESLINT_NAMED);
      if (s === 'espree') return r(ESPREE_NAMED);
      if (s === '../index.js') return { url: pathToFileURL(toolsRequire.resolve('eslint-plugin-unicorn')).href, shortCircuit: true };
      if (!s.startsWith('.') && !s.startsWith('node:')) {
        try {
          return { url: pathToFileURL(toolsRequire.resolve(s)).href, shortCircuit: true };
        } catch {}
      }
    }
    if (s === 'globals' || s === 'eslint-scope' || s === 'espree' || s.startsWith('@typescript-eslint/') || s === 'eslint' || s.startsWith('eslint/')) {
      try {
        return { url: pathToFileURL(toolsRequire.resolve(s)).href, shortCircuit: true };
      } catch {
        return r(RULE);
      }
    }
    return next(specifier, context);
  },
});
const each = (run) => {
  const f = (_n, fn) => run(fn);
  f.for = f.each = (cases) => (_n, fn) => cases.forEach((c) => (Array.isArray(c) ? fn(...c) : fn(c)));
  f.skip = () => {};
  f.only = f;
  return f;
};
globalThis.describe = each((fn) => fn());
// A test body that runs its own Linter (array-type's multi-pass fix checks) has nothing to
// record; let it fail without losing the file's RuleTester cases.
globalThis.it = globalThis.test = each((fn) => {
  try {
    fn();
  } catch {}
});

globalThis.__unicornTester = (importMeta, ruleId) => {
  const id = ruleId ?? path.basename(new URL(importMeta.url).pathname, '.js');
  const record = (tests, parser) => {
    const lo = { ...(tests.testerOptions?.languageOptions || {}) };
    if (parser) lo.parser = parser;
    globalThis.__recorded.push({
      name: 'unicorn/' + id,
      config: { languageOptions: { ...lo, globals: { ...unicornGlobals, ...(lo.globals || {}) } } },
      tests,
      defined: [],
    });
  };
  const test = (tests) => record(tests);
  test.snapshot = (tests) => record(tests);
  test.only = test;
  test.typescript = (tests) => record(tests, { __parser: 'ts' });
  for (const p of ['vue', 'html', 'svelte']) test[p] = (tests) => record(tests, { __parser: 'other' });
  return { ruleId: id, rule: unicornPlugin?.rules?.[id] ?? {}, test };
};

// ---- helpers -----------------------------------------------------------------------------
const parserKind = (p) => (p == null ? undefined : p.__parser || 'other');

function normalizeCase(c, valid) {
  if (typeof c === 'string') c = { code: c };
  const lo = c.languageOptions || {};
  return {
    name: c.name,
    code: c.code,
    filename: c.filename,
    options: c.options || [],
    settings: c.settings,
    languageOptions: lo,
    parser: parserKind(lo.parser),
    valid,
    expectedErrors: valid ? 0 : Array.isArray(c.errors) ? c.errors.length : c.errors,
    expectedOutput: c.output,
    skip: !!c.skip || !!c.dependencyConstraints,
    language: typeof c.language === 'string' ? c.language : c.language ? 'other' : undefined,
  };
}

function mergeLo(a = {}, b = {}) {
  const out = { ...a, ...b };
  out.parserOptions = { ...(a.parserOptions || {}), ...(b.parserOptions || {}) };
  if (a.parserOptions?.ecmaFeatures || b.parserOptions?.ecmaFeatures) {
    out.parserOptions.ecmaFeatures = { ...(a.parserOptions?.ecmaFeatures || {}), ...(b.parserOptions?.ecmaFeatures || {}) };
  }
  return out;
}

// typescript-eslint parser options that change scope analysis.
function pickParserOptions(po = {}) {
  const out = {};
  for (const k of ['jsxPragma', 'jsxFragmentName', 'emitDecoratorMetadata', 'experimentalDecorators', 'isolatedDeclarations', 'lib']) {
    if (po[k] !== undefined) out[k] = po[k];
  }
  return Object.keys(out).length ? out : undefined;
}

function applyFixes(code, fixes) {
  // Fix ranges index ESLint's text, which has no leading BOM; the BOM is kept in front.
  if (code.startsWith('\uFEFF')) return '\uFEFF' + applyFixes(code.slice(1), fixes);
  // One pass, like RuleTester: sort by range, skip overlapping fixes.
  const sorted = fixes.filter(Boolean).sort((x, y) => x.range[0] - y.range[0] || x.range[1] - y.range[1]);
  let out = '';
  let last = -Infinity;
  let pos = 0;
  for (const f of sorted) {
    if (f.range[0] <= last) continue;
    out += code.slice(pos, f.range[0]) + f.text;
    pos = f.range[1];
    last = f.range[1];
  }
  return out + code.slice(pos);
}

const requiresTypes = (ruleId) =>
  ruleId.startsWith('@typescript-eslint/') &&
  !!tsPlugin.rules[ruleId.slice('@typescript-eslint/'.length)]?.meta?.docs?.requiresTypeChecking;

function observe(ruleId, isTs, testerConfig, c) {
  if (c.parser === 'fixture' || c.parser === 'other' || c.skip || typeof c.code !== 'string') return { skipped: 'unsupported case' };
  if (c.language && c.language !== 'js/js') return { skipped: 'other language' };
  // Lone surrogates cannot be represented in UTF-8 source files.
  if (!c.code.isWellFormed()) return { skipped: 'lone surrogate' };
  const lo = mergeLo(testerConfig.languageOptions, c.languageOptions);
  const po = lo.parserOptions;
  const typed = requiresTypes(ruleId) && !!fixturesDir && !!(po.project || po.projectService);
  if (!typed) {
    if (po.project) return { skipped: 'type-aware case' };
    // Typed testers for rules that need no types: run without a program.
    delete po.projectService;
    delete po.tsconfigRootDir;
  }
  const merged = parserKind(lo.parser);
  if (merged === 'fixture' || merged === 'other') return { skipped: 'unsupported case' };
  const useTs = isTs || merged === 'ts';
  const jsx = !!lo.parserOptions?.ecmaFeatures?.jsx;
  let filename = c.filename
    ? path.basename(c.filename)
    : isTs
      ? jsx ? 'react.tsx' : 'file.ts'
      : 'file.js';
  let verifyName = filename;
  let typedInfo;
  if (typed) {
    const root = path.resolve(fixturesDir, po.tsconfigRootDir ?? fixturesDir);
    const abs = path.resolve(root, c.filename ?? (jsx ? 'react.tsx' : 'file.ts'));
    const rel = path.relative(fixturesDir, abs).split(path.sep).join('/');
    if (rel.startsWith('..')) return { skipped: 'file outside the fixtures' };
    verifyName = abs;
    filename = rel;
    typedInfo = {
      root: path.relative(fixturesDir, root).split(path.sep).join('/'),
      project: po.project ?? null,
      projectService: po.project ? undefined : po.projectService,
    };
    po.tsconfigRootDir = root;
  }
  const languageOptions = { ...lo };
  delete languageOptions.parser;
  if (useTs) languageOptions.parser = tsParser;
  const config = [
    {
      files: ['**/*.{js,mjs,cjs,jsx,ts,tsx,mts,cts}'],
      plugins: {
        '@typescript-eslint': tsPlugin,
        ...(jsdocPlugin ? { jsdoc: jsdocPlugin } : {}),
        ...(unicornPlugin ? { unicorn: unicornPlugin } : {}),
      },
      languageOptions,
      linterOptions: { reportUnusedDisableDirectives: 'off' },
      settings: c.settings || {},
      rules: { [ruleId]: ['error', ...c.options] },
    },
  ];
  const linter = new Linter({ configType: 'flat' });
  let messages;
  try {
    messages = linter.verify(c.code, config, { filename: verifyName });
  } catch (e) {
    return { skipped: 'eslint threw: ' + String(e.message).split('\n')[0] };
  }
  if (messages.some((m) => m.ruleId == null && /No matching configuration/.test(m.message))) return { skipped: 'no config' };
  if (messages.some((m) => m.fatal)) return { skipped: 'parse error: ' + messages.find((m) => m.fatal).message };
  const fixes = messages.map((m) => m.fix).filter(Boolean);
  const output = fixes.length ? applyFixes(c.code, fixes) : null;
  return {
    filename,
    typed: typedInfo,
    parser: useTs ? 'ts' : 'espree',
    languageOptions: {
      ecmaVersion: lo.ecmaVersion,
      // typescript-eslint testers often set the source type through `parserOptions`.
      sourceType: lo.sourceType ?? lo.parserOptions?.sourceType,
      globals: ruleId.startsWith('unicorn/') ? ownGlobals(lo.globals) : lo.globals,
      ecmaFeatures: lo.parserOptions?.ecmaFeatures,
      parserOptions: pickParserOptions(lo.parserOptions),
    },
    messages: messages.map((m) => ({
      ruleId: m.ruleId,
      messageId: m.messageId,
      message: m.message,
      line: m.line,
      column: m.column,
      endLine: m.endLine,
      endColumn: m.endColumn,
      fix: m.fix ? true : undefined,
      suggestions: m.suggestions?.map((s) => ({ messageId: s.messageId, desc: s.desc, output: applyFixes(c.code, [s.fix]) })),
    })),
    output,
  };
}

// A unicorn case's globals beyond the tester's defaults (written once per file).
function ownGlobals(g) {
  if (!g) return undefined;
  const own = Object.fromEntries(Object.entries(g).filter(([k, v]) => unicornGlobals[k] !== v));
  return Object.keys(own).length ? own : undefined;
}

function testFileFor(rule) {
  if (rule.startsWith('unicorn/')) {
    const f = unicornTests ? path.join(unicornTests, rule.slice('unicorn/'.length) + '.js') : '';
    return { files: fs.existsSync(f) ? [f] : [], isTs: false, source: 'eslint-plugin-unicorn' };
  }
  if (rule.startsWith('jsdoc/')) {
    const camel = rule.slice('jsdoc/'.length).replace(/-([a-z])/g, (_m, c) => c.toUpperCase());
    const f = jsdocTests ? path.join(jsdocTests, camel + '.js') : '';
    return { files: fs.existsSync(f) ? [f] : [], isTs: false, source: 'eslint-plugin-jsdoc' };
  }
  if (rule.startsWith('@typescript-eslint/')) {
    const base = rule.slice('@typescript-eslint/'.length);
    const files = [path.join(tsTests, base + '.test.ts')];
    const dir = path.join(tsTests, base);
    if (fs.existsSync(dir)) for (const f of fs.readdirSync(dir).sort()) if (f.endsWith('.test.ts')) files.push(path.join(dir, f));
    return { files: files.filter((f) => fs.existsSync(f)), isTs: true };
  }
  return { files: [path.join(eslintTests, rule + '.js')].filter((f) => fs.existsSync(f)), isTs: false };
}

fs.mkdirSync(outDir, { recursive: true });
// A RegExp in rule options (`ignore: [/^skip/]`) is written as `{ "regex": source,
// "flags": flags }`; plain JSON would turn it into `{}`.
function regexToJson(key, value) {
  return value instanceof RegExp ? { regex: value.source, flags: value.flags } : value;
}

for (const rule of rules) {
  const { files, isTs, source } = testFileFor(rule);
  if (!files.length) {
    console.error(`${rule}: no test file`);
    continue;
  }
  const cases = [];
  const skipped = {};
  for (const file of files) {
    globalThis.__recorded = [];
    try {
      const mod = await import(pathToFileURL(file).href + '?t=' + Date.now());
      // eslint-plugin-jsdoc's assertion files export the cases; its runner adds the version.
      if (source === 'eslint-plugin-jsdoc' && mod.default) {
        globalThis.__recorded.push({ name: rule, config: { languageOptions: { ecmaVersion: 'latest' } }, tests: mod.default, defined: [] });
      }
    } catch (e) {
      console.error(`${rule}: ${path.basename(file)} failed to load: ${String(e.message).split('\n')[0]}`);
      continue;
    }
    for (const rec of globalThis.__recorded) {
      for (const [list, valid] of [[rec.tests.valid || [], true], [rec.tests.invalid || [], false]]) {
        for (const raw of list) {
          const c = normalizeCase(raw, valid);
          if (rec.defined.some((d) => String(c.code).includes(d))) {
            skipped['uses test-only rule'] = (skipped['uses test-only rule'] || 0) + 1;
            continue;
          }
          const obs = observe(rule, isTs, rec.config, c);
          if (obs.skipped) {
            const k = obs.skipped.replace(/:.*/, '');
            skipped[k] = (skipped[k] || 0) + 1;
            continue;
          }
          cases.push({
            code: c.code,
            options: c.options.length ? c.options : undefined,
            settings: c.settings,
            valid: c.valid || undefined,
            ...obs,
          });
        }
      }
    }
  }
  const out = { rule, source: source ?? (isTs ? 'typescript-eslint' : 'eslint'), skipped, cases };
  if (rule.startsWith('unicorn/')) out.globals = unicornGlobals;
  const fname = rule.replace('@typescript-eslint/', 'ts-').replace('jsdoc/', 'jsdoc-').replace('unicorn/', 'unicorn-') + '.json';
  fs.writeFileSync(path.join(outDir, fname), JSON.stringify(out, regexToJson, 0).replace(/\},\{"code"/g, '},\n{"code"') + '\n');
  console.error(`${rule}: ${cases.length} cases, skipped ${JSON.stringify(skipped)}`);
}
fs.rmSync(stubDir, { recursive: true, force: true });
