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
//        [--import-tests <eslint-plugin-import>/tests/src/rules]
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
// out of each case's own; Vue, HTML, Svelte and non-JavaScript language cases are skipped;
// with `--fixtures`, its type-aware cases (`projectService`) run typed like typescript-eslint's,
// as files of the fixtures project, so run the generator from a directory above the fixtures
// copy: ESLint's cwd must contain the linted file),
// or `import/no-amd` (eslint-plugin-import 2.32.0 from its source tarball, the plugin itself
// in the tools dir: `tests/src/utils` and its RuleTester are replaced by a recorder, the
// test files are loaded as ES modules with `require`/`__dirname` shims, the plugin's
// eslintrc-style cases become flat config, the generator runs from the plugin root, and each
// case's `filename` is kept relative to `tests/files`; Babel and Flow parser cases are
// skipped, as are cases that need the webpack or TypeScript resolver (not installed: ESLint
// reports "Resolve error"); absolute paths into the plugin's checkout are written as
// `/import`, where the harness mounts it; a tester's `settings` apply to its cases; the
// rules get ESLint 9's `context.parserOptions` and `getTokenOrComment{Before,After}`, which
// ESLint 10 dropped; group-exports and no-relative-parent-imports, tested only with Babel,
// replay with espree). Don't create a `node_modules` in the plugin checkout, except for
// import/no-unused-modules: it lists files with ESLint 9's FileEnumerator (gone in ESLint
// 10), so generate it alone with an ESLint 9 tools dir (eslint@9 plus the checkout's
// eslintrc needs: eslint-plugin-eslint-plugin, eslint-plugin-json, babel-eslint, semver)
// symlinked as the checkout's `node_modules`, and remove the link after.

import { registerHooks, createRequire, isBuiltin } from 'node:module';
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
const importTestsOpt = opt('--import-tests');
const importTests = importTestsOpt ? fs.realpathSync(path.resolve(importTestsOpt)) : undefined;
// The plugin's checkout root: its tests name fixture files under `tests/files` from the cwd.
const importRoot = importTests ? path.resolve(importTests, '../../..') : undefined;
const importFiles = importRoot ? path.join(importRoot, 'tests/files') : undefined;
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
// eslint-plugin-import 2.32.0 targets ESLint 9: some rules read `context.parserOptions` and
// `sourceCode.getTokenOrComment{Before,After}`, which ESLint 10 removed. Each rule's context
// gets the ESLint 9 values back (flat config's `languageOptions.parserOptions`, and token
// lookups that include comments), so those rules behave as they do on ESLint 9.
const eslint9Context = (context) => {
  // Contexts are frozen: the extra properties go on an object that inherits from them.
  const sc = context.sourceCode;
  const sourceCode = Object.create(sc, {
    getTokenOrCommentBefore: { value: (n, skip) => sc.getTokenBefore(n, { includeComments: true, skip }) },
    getTokenOrCommentAfter: { value: (n, skip) => sc.getTokenAfter(n, { includeComments: true, skip }) },
  });
  return Object.create(context, {
    parserOptions: { value: context.languageOptions?.parserOptions ?? {} },
    sourceCode: { value: sourceCode },
    getSourceCode: { value: () => sourceCode },
  });
};
const importPluginRaw = importTests ? toolsRequire('eslint-plugin-import') : undefined;
const importPlugin = importPluginRaw && {
  ...importPluginRaw,
  rules: Object.fromEntries(
    Object.entries(importPluginRaw.rules).map(([n, r]) => [n, { ...r, create: (context) => r.create(eslint9Context(context)) }]),
  ),
};
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

// eslint-plugin-import's test helpers (`tests/src/utils.js`, `tests/src/rule-tester.js`):
// eslintrc-style cases (`parser` paths, `parserOptions`) recorded through
// `globalThis.__importTester`, which turns them into flat cases. Parsers are markers.
const IMPORT_UTILS = stub(
  'import-utils',
  `const path = require('path');
   const files = ${JSON.stringify(importFiles ?? stubDir)};
   const parsers = { ESPREE: 'tsr:espree', TS_OLD: false, TS_NEW: 'tsr:ts', BABEL_OLD: 'tsr:babel' };
   const testFilePath = (rel) => path.join(files, rel);
   const FILENAME = testFilePath('foo.js');
   const test = (t) => ({ filename: FILENAME, ...t, parserOptions: { sourceType: 'module', ecmaVersion: 9, ...t.parserOptions } });
   const yes = () => true;
   exports.parsers = parsers; exports.testFilePath = testFilePath; exports.FILENAME = FILENAME; exports.test = test;
   exports.testVersion = (_s, t) => test(t());
   exports.tsVersionSatisfies = yes; exports.typescriptEslintParserSatisfies = yes; exports.eslintVersionSatisfies = yes;
   exports.getTSParsers = () => [parsers.TS_NEW];
   exports.getNonDefaultParsers = () => [parsers.TS_NEW, parsers.BABEL_OLD];
   exports.testContext = (settings) => ({ getFilename: () => FILENAME, settings: settings || {} });
   exports.getFilename = (f) => path.join(files, f || 'foo.js');
   exports.SYNTAX_CASES = [];`,
);
const IMPORT_TESTER = stub(
  'import-tester',
  `class RuleTester {
     constructor(config) { this.config = config || { parserOptions: { sourceType: 'script' } }; }
     run(name, _rule, tests) { globalThis.__importTester(name, this.config, tests); }
   }
   exports.RuleTester = RuleTester;
   exports.usingFlatConfig = true;
   exports.withoutAutofixOutput = (t) => t;`,
);
const EMPTY = stub('empty', `exports.version = '0.0.0';`);
const CHAI = stub('chai', `exports.expect = () => new Proxy({}, { get: function g() { return new Proxy(() => {}, { get: g }); } });`);
const NO_PROCESS = stub(
  'no-process',
  `const no = () => { throw new Error('child processes are disabled in the oracle generator'); };
   exports.exec = no; exports.execSync = no; exports.execFile = no; exports.execFileSync = no;
   exports.spawn = no; exports.spawnSync = no; exports.fork = no;`,
);
const importRuleStubs = new Map();
const importRuleStub = (name) => {
  if (!importRuleStubs.has(name)) {
    importRuleStubs.set(
      name,
      stub('import-rule-' + name.replace(/[^\w-]/g, '_'), `module.exports = { meta: globalThis.__importRuleMeta(${JSON.stringify(name)}), create() { return {}; } };`),
    );
  }
  return importRuleStubs.get(name);
};
globalThis.__importRuleMeta = (name) => importPluginRaw?.rules?.[name]?.meta ?? {};

registerHooks({
  load(url, context, next) {
    // eslint-plugin-import's tests are ES modules that also use `require` and `__dirname`.
    if (importTests && url.startsWith(pathToFileURL(importTests).href)) {
      const file = new URL(url).pathname;
      const prelude =
        `import { createRequire as __tsrRequire } from 'node:module';` +
        `const require = __tsrRequire(import.meta.url);` +
        `const __filename = ${JSON.stringify(file)}; const __dirname = ${JSON.stringify(path.dirname(file))};`;
      return { format: 'module', source: prelude + fs.readFileSync(file, 'utf8'), shortCircuit: true };
    }
    return next(url, context);
  },
  resolve(specifier, context, next) {
    const s = specifier;
    // eslint-plugin-import's own lazy requires (no-unused-modules loads ESLint's
    // FileEnumerator while linting) resolve normally.
    if (/\/node_modules\/(eslint-plugin-import|eslint-module-utils)\//.test(context.parentURL ?? '')) return next(specifier, context);
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
    if (importTests && parent.startsWith(pathToFileURL(importTests).href)) {
      if (s === '../utils') return r(IMPORT_UTILS);
      if (s === '../rule-tester') return r(IMPORT_TESTER);
      // A rule module's stub carries the rule's `meta` (its message templates, which some
      // tests format their expected messages with), never its implementation.
      if (/^rules\//.test(s)) return r(importRuleStub(s.slice('rules/'.length)));
      // The fixture tree's own `package.json` files are data the tests read.
      if (/(^|\/)package\.json$/.test(s) && s.startsWith('.')) {
        const f = path.resolve(path.dirname(new URL(parent).pathname), s);
        if (importFiles && f.startsWith(importFiles + path.sep) && fs.existsSync(f)) return { url: pathToFileURL(f).href, shortCircuit: true };
      }
      if (/(^|\/)package\.json$/.test(s) && !s.startsWith('eslint/')) return r(EMPTY);
      if (s === 'chai') return r(CHAI);
      // Test bodies (`it`) are run for their RuleTester calls; never let one spawn a process.
      if (s === 'child_process' || s === 'node:child_process') return r(NO_PROCESS);
      if (['babel-preset-flow', 'babel-eslint'].includes(s)) return r(EMPTY);
      if (s.startsWith('.') && !path.extname(s)) {
        const f = path.resolve(path.dirname(new URL(parent).pathname), s + '.js');
        if (fs.existsSync(f)) return { url: pathToFileURL(f).href, shortCircuit: true };
      }
      if (!s.startsWith('.') && !isBuiltin(s)) {
        try {
          return { url: pathToFileURL(toolsRequire.resolve(s)).href, shortCircuit: true };
        } catch {
          // A parser or package the cases need that isn't installed: its cases are skipped.
          return r(OTHER_PARSER);
        }
      }
    }
    if (unicornTests && parent.startsWith(pathToFileURL(unicornTests).href)) {
      if (/(^|\/)utils\/test\.js$/.test(s)) return r(UNICORN_TEST);
      if (/scripts\/parsers\.js$/.test(s)) return r(UNICORN_PARSERS);
      if (s === 'node:test') return r(NODE_TEST);
      if (s === 'eslint') return r(ESLINT_NAMED);
      if (s === 'espree') return r(ESPREE_NAMED);
      if (s === '../index.js') return { url: pathToFileURL(toolsRequire.resolve('eslint-plugin-unicorn')).href, shortCircuit: true };
      // A test that imports its rule module (for its messages) gets the stub.
      if (/^\.\.\/rules\//.test(s)) return r(RULE);
      if (!s.startsWith('.') && !s.startsWith('node:')) {
        try {
          return { url: pathToFileURL(toolsRequire.resolve(s)).href, shortCircuit: true };
        } catch {
          // Language plugins for cases of other languages (CSS, JSON, Markdown, ...), which
          // are skipped.
          if (/^(@eslint\/(css|json|markdown)|@html-eslint\/|eslint-plugin-(toml|yml|yaml)|eslint-soml)/.test(s)) return r(EMPTY);
        }
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
// mocha's `this` in suites and hooks, and its hooks: `before*` run at once (fixture files a
// suite creates must exist when its cases are replayed), `after*` never (so they stay).
const mochaThis = { timeout() {}, slow() {}, retries() {}, skip() {} };
globalThis.describe = each((fn) => fn.call(mochaThis));
// mocha's alias (eslint-plugin-import's tests).
globalThis.context = globalThis.describe;
globalThis.before = globalThis.beforeEach = (fn) => {
  try {
    fn.call(mochaThis);
  } catch {}
};
globalThis.after = globalThis.afterEach = () => {};
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
    // A tester for another language (`testerOptions.language`: CSS, JSON, YAML, ...).
    if (tests.testerOptions && 'language' in tests.testerOptions) lo.parser = { __parser: 'other' };
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

// eslint-plugin-import's eslintrc-style cases as flat ones (as its own FlatCompatRuleTester
// does): `parser` (a marker or module path), `parserOptions.ecmaVersion/sourceType` move to
// `languageOptions`.
// group-exports and no-relative-parent-imports test only with Babel; their cases are
// replayed with espree (the default parser), and those that need Babel syntax (Flow) fail
// to parse and are skipped.
const BABEL_AS_ESPREE = new Set(['group-exports', 'no-relative-parent-imports']);
let babelAsEspree = false;
const importParser = (p) => {
  if (p == null) return undefined;
  if (typeof p !== 'string') return p;
  if (p === 'tsr:espree' || /espree/.test(p)) return undefined;
  if (babelAsEspree && (p === 'tsr:babel' || /babel-eslint|@babel\/eslint-parser/.test(p))) return undefined;
  if (p === 'tsr:ts' || /typescript-eslint|ts-parser/.test(p)) return { __parser: 'ts' };
  return { __parser: 'other' };
};
const importFlat = (t) => {
  if (typeof t === 'string') t = { code: t };
  const { parser, parserOptions = {}, languageOptions = {}, ...rest } = t;
  const { ecmaVersion, sourceType, ...po } = parserOptions;
  const lo = { ...languageOptions };
  if (ecmaVersion !== undefined) lo.ecmaVersion = ecmaVersion;
  if (sourceType !== undefined) lo.sourceType = sourceType;
  if (Object.keys(po).length) lo.parserOptions = { ...(lo.parserOptions || {}), ...po };
  const pk = importParser(parser);
  if (pk) lo.parser = pk;
  return { ...rest, languageOptions: lo };
};
globalThis.__importTester = (name, config, tests) => {
  babelAsEspree = BABEL_AS_ESPREE.has(name);
  // A tester's own `settings` (config/typescript.js) apply to each of its cases.
  const withSettings = (t) => {
    const f = importFlat(t);
    return config?.settings ? { ...f, settings: { ...config.settings, ...(f.settings || {}) } } : f;
  };
  globalThis.__recorded.push({
    name: 'import/' + name,
    config: { languageOptions: importFlat(config).languageOptions },
    tests: { valid: (tests.valid || []).map(withSettings), invalid: (tests.invalid || []).map(withSettings) },
    defined: [],
  });
};
if (importRoot) process.chdir(importRoot);

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
  // eslint-plugin-unicorn rules use types when the case gives a program (`projectService`).
  const typed = (requiresTypes(ruleId) || ruleId.startsWith('unicorn/')) && !!fixturesDir && !!(po.project || po.projectService);
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
  // eslint-plugin-import's cases name files in its fixture tree (`tests/files`), which its
  // rules read; the recorded name is relative to that tree.
  const importFile = ruleId.startsWith('import/') && c.filename && importFiles && path.isAbsolute(c.filename);
  let filename = importFile
    ? path.relative(importFiles, c.filename).split(path.sep).join('/')
    : c.filename
    ? path.basename(c.filename)
    : isTs
      ? jsx ? 'react.tsx' : 'file.ts'
      : 'file.js';
  let verifyName = importFile ? c.filename : filename;
  let typedInfo;
  if (typed) {
    // The plugin's tester lets its default project cover `file.*`; the fixtures' tsconfig
    // already includes `file.ts`, which the project service then finds by itself.
    if (ruleId.startsWith('unicorn/') && po.projectService) po.projectService = true;
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
        ...(importPlugin ? { import: importPlugin } : {}),
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
  // The webpack and TypeScript resolvers aren't installed: ESLint reports only that.
  if (messages.some((m) => m.message.startsWith('Resolve error: unable to load resolver'))) {
    return { skipped: 'resolver plugin not installed' };
  }
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
  if (rule.startsWith('import/')) {
    const f = importTests ? path.join(importTests, rule.slice('import/'.length) + '.js') : '';
    return { files: fs.existsSync(f) ? [f] : [], isTs: false, source: 'eslint-plugin-import' };
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
// eslint-plugin-import's cases name files of its checkout by absolute path (in code,
// options, settings and messages). The harness mounts the checkout's `tests` at `/import`,
// so the checkout root is written as `/import`; columns after a rewritten path in the
// code move left by the length difference.
const MOUNT = '/import';
function unmachine(rec, root) {
  const lines = rec.code.split(/\r\n|[\r\n\u2028\u2029]/);
  const shift = (line, col) => {
    if (line == null || col == null) return col;
    const before = (lines[line - 1] ?? '').slice(0, col - 1);
    return col - (before.split(root).length - 1) * (root.length - MOUNT.length);
  };
  for (const m of rec.messages ?? []) {
    m.column = shift(m.line, m.column);
    m.endColumn = shift(m.endLine, m.endColumn);
  }
  const map = (v) => {
    if (typeof v === 'string') return v.split(root).join(MOUNT);
    if (Array.isArray(v)) return v.map(map);
    if (v && typeof v === 'object' && !(v instanceof RegExp)) return Object.fromEntries(Object.entries(v).map(([k, x]) => [map(k), map(x)]));
    return v;
  };
  return map(rec);
}

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
            // GEN_DEBUG=1 prints each skip reason in full.
            if (process.env.GEN_DEBUG) console.error(`${rule}: ${obs.skipped}`);
            skipped[k] = (skipped[k] || 0) + 1;
            continue;
          }
          const entry = {
            code: c.code,
            options: c.options.length ? c.options : undefined,
            settings: c.settings,
            valid: c.valid || undefined,
            ...obs,
          };
          // Lone surrogates (in options or messages too) have no UTF-8 form.
          const wellFormed = (v) =>
            typeof v === 'string' ? v.isWellFormed() : v && typeof v === 'object' ? Object.values(v).every(wellFormed) : true;
          if (!wellFormed(entry)) {
            skipped['lone surrogate'] = (skipped['lone surrogate'] || 0) + 1;
            continue;
          }
          cases.push(importRoot ? unmachine(entry, importRoot) : entry);
        }
      }
    }
  }
  const out = { rule, source: source ?? (isTs ? 'typescript-eslint' : 'eslint'), skipped, cases };
  if (rule.startsWith('unicorn/')) out.globals = unicornGlobals;
  const fname = rule.replace('@typescript-eslint/', 'ts-').replace('jsdoc/', 'jsdoc-').replace('unicorn/', 'unicorn-').replace('import/', 'import-') + '.json';
  fs.writeFileSync(path.join(outDir, fname), JSON.stringify(out, regexToJson, 0).replace(/\},\{"code"/g, '},\n{"code"') + '\n');
  console.error(`${rule}: ${cases.length} cases, skipped ${JSON.stringify(skipped)}`);
}
fs.rmSync(stubDir, { recursive: true, force: true });
