// The six tools: the home page's tool list and each /tools/<slug>/ page. The words come from the
// docs pages (content/tools/*.md; scripts/check.mjs keeps the --help excerpts in step); every number on a tool page is read from data/bench.json
// through `sections` and `correctness`, never typed in here.

export type Tool = {
  name: string;
  /** What it is, in a few words: the tool page's title after the name. */
  title: string;
  replaces: string;
  /** The home page card. */
  body: string;
  /** The tool page's opening paragraph. */
  intro: string;
  works: string[];
  notYet: string[];
  /** A short --help excerpt. */
  usage: string;
  /** How switching looks: a diff of the file that names the tool today. */
  diff: { file: string; before: string[]; changes: [string, string][]; after: string[] };
  /** The "next to" section: how to adopt it one tool at a time. */
  nextTo: { title: string; body: string };
  /** data/bench.json sections drawn on the page, in order. */
  sections: string[];
  /** Rows of the bench.json correctness table that belong to this tool (matched on "What"). */
  correctness: string[];
  /** The shared crates it is built on, for the engine diagram. */
  crates: string[];
};

export const TOOLS: Tool[] = [
  {
    name: 'ts-r-dev',
    title: 'dev server with hot reload',
    replaces: 'the Vite dev server',
    body: 'Serves your app as unbundled ES modules, transforms only the file you save and pushes it to the browser. Speaks Vite’s hot-reload protocol, so import.meta.hot code works unchanged.',
    intro:
      'ts-r-dev serves your app as unbundled ES modules, transforms only the file you save, and pushes the update to the browser. It speaks Vite’s hot-reload protocol, so code written against import.meta.hot works unchanged. It replaces vite dev for the dev loop, not vite build.',
    works: [
      'Hot module replacement with Vite’s semantics: self-accepting modules, hot.accept, hot.decline, hot.invalidate, hot.data, hot.dispose and hot.on',
      'TypeScript, TSX and JSX through ts-r’s own transform; JSON, assets and CSS, with CSS updates applied without a reload',
      'React Fast Refresh when a React Refresh runtime is installed; top-level components keep their state across edits',
      'Dependencies pre-bundled in one code-split batch at startup, cached per lockfile',
      'An error overlay for transform and resolve errors; the server never crashes on a bad file',
      'pnpm workspace packages treated as source and hot-updated like app code',
      'vite.config loaded in a Node sidecar: Vite plugins’ resolveId, load, transform, transformIndexHtml, handleHotUpdate and configureServer hooks, plus mode, base, define, env files and server options',
      'HTTPS, a proxy, CORS and custom headers from server.* in vite.config; --open, --host, --mode and --base',
    ],
    notYet: [
      'No CSS preprocessors (Sass, Less), and no css.*, server.hmr, server.fs or server.watch config',
      'CSS @import and CSS modules in app CSS aren’t handled',
      'Inline module scripts in HTML and non-literal import(expr) aren’t rewritten',
      'No compression; HTTP/1.1 only',
      'Windows is untested',
    ],
    usage: `ts-r-dev: dev server with hot module replacement (replaces vite dev)

Usage: ts-r-dev [root] [options]

  --port <n>         Port to listen on (default 5173, or server.port)
  --host [addr]      Address to bind (default 127.0.0.1)
  --open [path]      Open the browser once the server is ready
  --no-cache         Keep transform results in memory only
  --force            Pre-bundle dependencies again
  --no-plugins       Don't load vite.config plugins`,
    diff: { file: 'package.json', before: ['"scripts": {'], changes: [['"dev": "vite",', '"dev": "ts-r-dev",']], after: ['}'] },
    nextTo: {
      title: 'Next to Vite',
      body: 'Use ts-r-dev for the dev loop and keep Vite, or anything else, for production builds. It reads your index.html, package.json and vite.config, and runs your Vite plugins in a Node sidecar. The client runtime is served at both /@ts-r/client and /@vite/client and its messages match Vite’s, so framework code that listens to vite: events keeps working.',
    },
    sections: ['dev-vs-vite', 'hmr'],
    correctness: [],
    crates: ['lexer', 'parser', 'ast', 'config', 'resolver', 'cache', 'transform', 'codegen', 'bundler', 'hmr'],
  },
  {
    name: 'ts-r-build',
    title: 'bundler and minifier',
    replaces: 'esbuild, Rollup, Vite build',
    body: 'A production bundler with esbuild’s command line: ESM, CommonJS and IIFE output, code splitting, tree shaking, minifying and source maps.',
    intro:
      'ts-r-build bundles and minifies TypeScript and JavaScript for production. Its command line follows esbuild 0.28.2, so most esbuild commands work after swapping the binary name. Like esbuild, it strips types without checking them.',
    works: [
      'Bundling ES modules and CommonJS, with tree shaking that honours sideEffects and /*#__PURE__*/',
      'Output formats esm, cjs and iife, and code splitting with --splitting',
      'Platforms browser, node and neutral; tsconfig paths and baseUrl',
      'Minification of whitespace, identifiers and syntax',
      'Source maps: linked, external, inline or both',
      'JSX with the classic or the automatic runtime, --watch and a persistent build cache',
    ],
    notYet: [
      'No plugins and no JavaScript config file or API',
      'No CSS bundling; @import is not followed',
      'No syntax lowering: targets below ES2022 are rejected',
      'No --mangle-props, --drop, --pure or --keep-names',
      'Minified output is about 3% larger than esbuild’s',
    ],
    usage: `Usage: ts-r-build [options] [entry points]

  --bundle          Bundle all dependencies into the output files
  --format=...      Output format (iife | cjs | esm)
  --minify          Minify the output
  --outdir=...      The output directory
  --platform=...    Platform target (browser | node | neutral)
  --sourcemap       Emit a source map
  --splitting       Enable code splitting (requires --format=esm)
  --watch           Rebuild when a source file changes`,
    diff: { file: 'package.json', before: ['"scripts": {'], changes: [['"build": "esbuild src/app.ts --bundle --minify --outdir=dist",', '"build": "ts-r-build src/app.ts --bundle --minify --outdir=dist",']], after: ['}'] },
    nextTo: {
      title: 'Next to esbuild and Vite',
      body: 'Use ts-r-build where you call the esbuild command line today, or to build a library while the app keeps Vite. It has no JavaScript API yet, so scripts that call esbuild.build() from Node switch to the command line first. ts-r-dev uses the same bundler to pre-bundle an app’s dependencies.',
    },
    sections: ['build'],
    correctness: ['Bundler'],
    crates: ['lexer', 'parser', 'ast', 'config', 'resolver', 'cache', 'transform', 'codegen', 'bundler', 'minify'],
  },
  {
    name: 'ts-r-check',
    title: 'type checker',
    replaces: 'tsc',
    body: 'A type checker that aims for the same errors, codes and messages as TypeScript 7, and keeps a history so it re-checks only what changed.',
    intro:
      'ts-r-check type-checks a TypeScript project and prints diagnostics in tsc’s format, with tsc’s error codes. Like tsc, it writes JavaScript, source maps and declarations unless noEmit is set; --noEmit only checks. It is the least finished part of ts-r, and it follows one rule: no false positives first.',
    works: [
      'Loading the project from tsconfig.json, including extends, with tsc’s config diagnostics',
      'Module resolution that matches tsc’s on every resolution of the reference app',
      'Assignability, calls and overloads, generics and inference, unions and narrowing',
      'keyof, mapped, conditional and literal types; classes, declarations and unused-code checks',
      'tsc’s output in both plain and pretty form, with tsc’s exit codes',
    ],
    notYet: [
      'Octal escapes in strings (TS1487) only with --no-history; no “Did you mean” suggestions on regular expression errors (TS1369)',
      'Some inference: contextual types through reverse mapped types',
      'Some relation and narrowing results, mostly in TS2322, TS2339 and TS2345',
      '424 of 12,750 of tsc’s conformance test variants still differ (3.3%)',
    ],
    usage: `usage: ts-r-check [-p tsconfig.json] [options]`,
    diff: { file: 'package.json', before: ['"scripts": {'], changes: [['"typecheck": "tsc --noEmit -p tsconfig.json",', '"typecheck": "ts-r-check --noEmit -p tsconfig.json",']], after: ['}'] },
    nextTo: {
      title: 'Next to tsc',
      body: 'ts-r-check reads the same tsconfig.json and prints the same format, so editor problem matchers and CI annotations that parse tsc’s output work unchanged. An error from ts-r-check is very likely one tsc reports too, but a clean run doesn’t mean tsc is clean. Keep tsc in CI until ts-r-check covers the features your project uses.',
    },
    sections: ['check'],
    correctness: ['Module resolution', 'tsconfig loading', 'Binder diagnostics', 'Type checker'],
    crates: ['lexer', 'parser', 'ast', 'config', 'resolver', 'cache', 'binder', 'checker'],
  },
  {
    name: 'ts-r-lint',
    title: 'linter',
    replaces: 'ESLint + typescript-eslint',
    body: 'A linter that reads your ESLint flat config, runs 154 ESLint and typescript-eslint rules, 51 of them type-aware, and honours eslint-disable comments.',
    intro:
      'ts-r-lint runs ESLint-compatible rules over JavaScript and TypeScript. It reads the config you already have, reports in ESLint’s formats and exits with ESLint’s codes, so it can stand in for eslint in scripts and CI when your config only uses the rules it has.',
    works: [
      '154 rules: all 64 of ESLint 10’s recommended set and typescript-eslint’s recommended and strict rules; 51 are type-aware, on ts-r’s own checker',
      '15,495 of 15,517 cases of the published ESLint and typescript-eslint tests for those rules match',
      'ESLint flat config, .oxlintrc.json and biome.json',
      '--fix, eslint-disable comments and inline config, with unused-disable reports',
      'Output as stylish, json, sarif or GitHub Actions annotations',
    ],
    notYet: [
      'Type-aware rules only where ts-r-check matches your tsc on the project; elsewhere they are skipped and listed on stderr',
      'No ESLint plugins other than typescript-eslint',
    ],
    usage: `ts-r-lint [options] [paths...]

  -f, --format <name>      stylish (default), json, sarif, github
  -c, --config <path>      Use this config for every file
      --fix                Apply fixes
      --max-warnings <n>   Exit 1 above this many warnings
      --list-rules         Print the rules ts-r-lint has`,
    diff: { file: 'package.json', before: ['"scripts": {'], changes: [['"lint": "eslint .",', '"lint": "ts-r-lint .",'], ['"lint:fix": "eslint --fix .",', '"lint:fix": "ts-r-lint --fix .",']], after: ['}'] },
    nextTo: {
      title: 'Next to ESLint',
      body: 'A common split: run ts-r-lint on every save or commit for the fast rules, and keep ESLint in CI for type-aware rules and plugins ts-r-lint doesn’t have yet. In the editor, ts-r-lsp shows the same diagnostics.',
    },
    sections: ['lint-same-rules', 'lint'],
    correctness: ['Lint rules'],
    crates: ['lexer', 'parser', 'ast', 'config', 'cache', 'lint'],
  },
  {
    name: 'ts-r-run',
    title: 'run TypeScript with Node',
    replaces: 'tsx, ts-node',
    body: 'Runs a TypeScript file on the Node you already have, with tsconfig paths, source maps and --watch.',
    intro:
      'ts-r-run compiles the files an entry imports, caches them, and starts Node with a small preload that serves the compiled code. It replaces tsx and ts-node for scripts, servers and tests. Like them, it doesn’t check types.',
    works: [
      '.ts, .tsx, .mts and .cts entries, with ES modules and CommonJS in the same graph',
      'Enums, namespaces, parameter properties and TC39 decorators',
      'tsconfig paths and JSX settings; stack traces mapped back to TypeScript',
      '--watch, which restarts Node when a file in the graph changes',
      'A warm start that only checks file sizes and times before handing over to Node',
      'Emit that matches tsc 7 on every file of the reference app',
    ],
    notYet: [
      'The cache check doesn’t notice a new file that would change a resolution',
      'Only the entry’s tsconfig.json is used for every file, as with tsx',
      'Windows is untested',
    ],
    usage: `ts-r-run: run a TypeScript file with Node

Usage: ts-r-run [options] [node options] <file> [args...]

  --watch               Restart when a file of the module graph changes
  --no-cache            Compile everything again
  --no-source-maps      Faster start on large graphs

Needs Node 22.15+ (module.registerHooks).`,
    diff: { file: 'package.json', before: ['"scripts": {'], changes: [['"seed": "tsx scripts/seed.ts",', '"seed": "ts-r-run scripts/seed.ts",'], ['"serve": "tsx watch src/server.ts",', '"serve": "ts-r-run --watch src/server.ts",']], after: ['}'] },
    nextTo: {
      title: 'Next to tsx and Node',
      body: 'ts-r-run needs nothing installed in the project and runs on the Node you already have. Unlike Node’s own type stripping, it handles enums, namespaces, parameter properties, decorators and tsconfig paths. Package imports stay with Node, so exports and runtime conditions behave exactly as they do without ts-r.',
    },
    sections: ['run'],
    correctness: ['JavaScript output (emit)'],
    crates: ['lexer', 'parser', 'ast', 'config', 'resolver', 'cache', 'transform', 'codegen'],
  },
  {
    name: 'ts-r-lsp',
    title: 'language server',
    replaces: 'tsserver and the ESLint server',
    body: 'One language server per workspace, shared by every editor window, for types and lint together, with a memory budget.',
    intro:
      'ts-r-lsp gives every editor window on a workspace one shared server instead of a TypeScript process each. Each window starts a small front process, about 3 MB, that connects to one daemon per workspace. The daemon runs the type backend, watches memory, and adds lint diagnostics from ts-r-lint.',
    works: [
      'One daemon per workspace, shared by every window',
      'A memory budget: the backend restarts when idle above it, and open documents are replayed',
      'The ts-r checker or your own TypeScript as the type backend, with a per-project parity check',
      'Lint diagnostics and code actions: fixes, suggestions, Fix all and disable comments',
      'A tsserver stand-in so VS Code’s built-in TypeScript support can share the daemon',
    ],
    notYet: [
      'Slower than TypeScript 7’s own LSP to first diagnostics, and per keystroke on two of three measured projects',
      '22 of 24 reference-app projects fall back to tsserver for diagnostics',
      'The VS Code extension isn’t packaged yet',
      'Windows is untested',
    ],
    usage: `ts-r-lsp: one shared, memory-budgeted language server per workspace

  ts-r-lsp [--stdio] [OPTIONS]     LSP server on stdio
  ts-r-lsp tsserver [ARGS]         tsserver stand-in on stdio
  ts-r-lsp status [--root DIR]     print the daemon's status as JSON
  ts-r-lsp stop [--root DIR]       ask the daemon to exit`,
    diff: { file: 'languages.toml (Helix)', before: ['[language-server.typescript]'], changes: [['command = "typescript-language-server"', 'command = "ts-r-lsp"']], after: ['args = ["--stdio"]'] },
    nextTo: {
      title: 'Next to tsserver',
      body: 'Editors other than VS Code start ts-r-lsp --stdio as an ordinary language server. When a project opens, the daemon runs your own tsc in the background and compares results; where they differ, diagnostics come from tsserver while hover, definition and the rest stay on ts-r.',
    },
    sections: ['editor', 'editor-lint', 'editor-memory'],
    correctness: [],
    crates: ['lexer', 'parser', 'ast', 'config', 'resolver', 'cache', 'binder', 'checker', 'lint'],
  },
];

export const toolBySlug = (slug: string) => TOOLS.find(t => t.name === slug);
