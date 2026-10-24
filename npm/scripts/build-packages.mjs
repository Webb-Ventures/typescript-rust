#!/usr/bin/env node
// Assembles every npm package from cargo build output. Dependency-free; Node >= 18.
//
//   cargo build --release -p ts_r_cli -p ts_r_run ... [--target <triple>]
//   node npm/scripts/build-packages.mjs [options]
//
// Options:
//   --target <triple>    Rust target to package (repeatable). Default: the host target.
//   --bin-dir <dir>      Directory holding the binaries (one --target only). Default: cargo's
//                        <target-dir>/<triple>/<profile>, else <target-dir>/<profile> for the host.
//   --artifacts <dir>    Binaries per target in <dir>/<triple>/ (the release workflow's layout).
//   --profile <name>     Cargo profile directory: release (default), dist or debug.
//   --target-dir <dir>   Cargo target dir. Default: <repo>/target.
//   --lib-dir <dir>      TypeScript lib.*.d.ts files for ts-r-check and ts-r-lsp. Default: the
//                        synced copy in target/upstream (cargo xtask upstream sync --minimal).
//   --out <dir>          Output dir. Default: <repo>/npm/dist.
//   --version <x.y.z>    ts-r version. Default: [workspace.package] version in Cargo.toml.
//   --repository <o/r>   GitHub repository for package.json (must match the publishing repo for
//                        npm provenance). Default: $GITHUB_REPOSITORY, else Webb-Ventures/typescript-rust.
//
// Output (<out>/packages.json lists them in publish order):
//   platforms/<tool>-<platform>/  one per tool and target, e.g. @ts-r.dev/lint-darwin-arm64 with
//                                 bin/ts-r-lint (check and lsp also carry lib/lib.*.d.ts)
//   fronts/<tool>/                @ts-r.dev/run, @ts-r.dev/dev, @ts-r.dev/build, @ts-r.dev/check,
//                                 @ts-r.dev/lint, @ts-r.dev/lsp: a JS shim plus their platform
//                                 packages as optionalDependencies, so each installs only its
//                                 own binary
//   fronts/ts-r/                  @ts-r.dev/cli, the `ts-r` launcher: @ts-r.dev/cli-<platform> plus every front
//   fronts/typescript/            @ts-r.dev/typescript, the drop-in for the typescript package

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const npmDir = path.resolve(here, '..');
const repo = path.resolve(npmDir, '..');

// Rust target -> npm platform. `key` is Node's platform key (PLATFORMS in npm/ts-r/bin/ts-r.js
// and npm/typescript/lib/real.cjs), `suffix` the package name suffix.
export const TARGETS = {
  'aarch64-apple-darwin': { suffix: 'darwin-arm64', key: 'darwin-arm64', os: 'darwin', cpu: 'arm64' },
  'x86_64-apple-darwin': { suffix: 'darwin-x64', key: 'darwin-x64', os: 'darwin', cpu: 'x64' },
  'aarch64-unknown-linux-gnu': { suffix: 'linux-arm64-gnu', key: 'linux-arm64-gnu', os: 'linux', cpu: 'arm64', libc: 'glibc' },
  'aarch64-unknown-linux-musl': { suffix: 'linux-arm64-musl', key: 'linux-arm64-musl', os: 'linux', cpu: 'arm64', libc: 'musl' },
  'x86_64-unknown-linux-gnu': { suffix: 'linux-x64-gnu', key: 'linux-x64-gnu', os: 'linux', cpu: 'x64', libc: 'glibc' },
  'x86_64-unknown-linux-musl': { suffix: 'linux-x64-musl', key: 'linux-x64-musl', os: 'linux', cpu: 'x64', libc: 'musl' },
  'aarch64-pc-windows-msvc': { suffix: 'win32-arm64-msvc', key: 'win32-arm64', os: 'win32', cpu: 'arm64', exe: '.exe' },
  'x86_64-pc-windows-msvc': { suffix: 'win32-x64-msvc', key: 'win32-x64', os: 'win32', cpu: 'x64', exe: '.exe' },
};

// One entry per binary. `front` is the npm package users install.
export const TOOLS = [
  { id: 'cli', binary: 'ts-r', front: '@ts-r.dev/cli', about: 'the ts-r launcher' },
  { id: 'run', binary: 'ts-r-run', front: '@ts-r.dev/run', about: 'run TypeScript on your Node (replaces tsx, ts-node)' },
  { id: 'dev', binary: 'ts-r-dev', front: '@ts-r.dev/dev', about: 'dev server with HMR (replaces vite dev)' },
  { id: 'build', binary: 'ts-r-build', front: '@ts-r.dev/build', about: 'production bundler with esbuild\'s flags' },
  { id: 'check', binary: 'ts-r-check', front: '@ts-r.dev/check', about: 'type checker with tsc\'s flags and error codes', libs: true, version: false, matcher: true },
  { id: 'lint', binary: 'ts-r-lint', front: '@ts-r.dev/lint', about: 'linter that reads your ESLint, oxlint or Biome config' },
  { id: 'lsp', binary: 'ts-r-lsp', front: '@ts-r.dev/lsp', about: 'language server for editors', libs: true },
];

function die(msg) {
  process.stderr.write(`build-packages: ${msg}\n`);
  process.exit(1);
}

function parseArgs(argv) {
  const opts = {
    targets: [],
    binDir: null,
    artifacts: null,
    profile: 'release',
    targetDir: path.join(repo, 'target'),
    libDir: path.join(repo, 'target/upstream/typescript/tsc/internal/bundled/libs'),
    out: path.join(npmDir, 'dist'),
    version: null,
    repository: process.env.GITHUB_REPOSITORY || 'Webb-Ventures/typescript-rust',
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const value = () => {
      const v = argv[++i];
      if (v === undefined) die(`${a} needs a value`);
      return v;
    };
    switch (a) {
      case '--target': opts.targets.push(value()); break;
      case '--bin-dir': opts.binDir = path.resolve(value()); break;
      case '--artifacts': opts.artifacts = path.resolve(value()); break;
      case '--profile': opts.profile = value(); break;
      case '--target-dir': opts.targetDir = path.resolve(value()); break;
      case '--lib-dir': opts.libDir = path.resolve(value()); break;
      case '--out': opts.out = path.resolve(value()); break;
      case '--version': opts.version = value(); break;
      case '--repository': opts.repository = value(); break;
      case '-h':
      case '--help': {
        const src = fs.readFileSync(fileURLToPath(import.meta.url), 'utf8');
        process.stdout.write(src.split('\n').filter((l) => l.startsWith('//')).map((l) => l.slice(3)).join('\n') + '\n');
        process.exit(0);
      }
      default: die(`unknown argument ${a} (see --help)`);
    }
  }
  if (opts.binDir && opts.targets.length > 1) die('--bin-dir takes a single --target');
  return opts;
}

function hostTarget() {
  try {
    const out = execFileSync('rustc', ['-vV'], { cwd: repo, encoding: 'utf8' });
    const m = /^host: (\S+)$/m.exec(out);
    if (m) return m[1];
  } catch (_) {
    // No rustc on PATH: derive it from Node's view of the platform.
  }
  const guess = Object.entries(TARGETS).find(([, t]) => t.os === process.platform && t.cpu === process.arch && (t.os !== 'linux' || t.libc === 'glibc'));
  if (!guess) die(`can't determine the host target for ${process.platform}-${process.arch}; pass --target`);
  return guess[0];
}

function workspaceVersion() {
  const toml = fs.readFileSync(path.join(repo, 'Cargo.toml'), 'utf8');
  const section = toml.split(/^\[/m).find((s) => s.startsWith('workspace.package]'));
  const m = section && /^version\s*=\s*"([^"]+)"/m.exec(section);
  if (!m) die('no [workspace.package] version in Cargo.toml; pass --version');
  return m[1];
}

const readJson = (p) => JSON.parse(fs.readFileSync(p, 'utf8'));
const writeJson = (p, value) => fs.writeFileSync(p, JSON.stringify(value, null, 2) + '\n');

/// The shim, the drop-in and this script must agree on the platforms.
function checkConsistency() {
  const sources = ['ts-r/bin/ts-r.js', 'typescript/lib/real.cjs'];
  for (const rel of sources) {
    const text = fs.readFileSync(path.join(npmDir, rel), 'utf8');
    const block = /const PLATFORMS = \{([^}]*)\}/.exec(text);
    if (!block) die(`${rel}: no PLATFORMS table`);
    const listed = [...block[1].matchAll(/'([^']+)': '([^']+)'/g)].map((m) => `${m[1]}=${m[2]}`).sort();
    const ours = Object.values(TARGETS).map((t) => `${t.key}=${t.suffix}`).sort();
    if (JSON.stringify(listed) !== JSON.stringify(ours)) die(`${rel} PLATFORMS ${JSON.stringify(listed)} != TARGETS ${JSON.stringify(ours)}`);
  }
}

/// @ts-r.dev/typescript's version: TypeScript's major.minor, so `npm:@ts-r.dev/typescript@^7` works, and
/// a patch that encodes the ts-r version (0.3.1 -> 3001, 1.2.3 -> 1002003; prerelease kept).
export function dropInVersion(tsVersion, tsrVersion) {
  const [tsMajor, tsMinor] = tsVersion.split('.');
  const m = /^(\d+)\.(\d+)\.(\d+)(-.+)?$/.exec(tsrVersion);
  if (!m) die(`bad ts-r version ${tsrVersion}`);
  const patch = Number(m[1]) * 1_000_000 + Number(m[2]) * 1000 + Number(m[3]);
  return `${tsMajor}.${tsMinor}.${patch}${m[4] || ''}`;
}

/// ts-r's MIT license, shipped in every package.
function copyLicense(dest) {
  fs.copyFileSync(path.join(repo, 'LICENSE'), path.join(dest, 'LICENSE'));
}

function binaryDir(opts, triple, host) {
  if (opts.binDir) return opts.binDir;
  if (opts.artifacts) return path.join(opts.artifacts, triple);
  const crossDir = path.join(opts.targetDir, triple, opts.profile);
  if (fs.existsSync(crossDir)) return crossDir;
  if (triple === host) return path.join(opts.targetDir, opts.profile);
  die(`no build output for ${triple} in ${crossDir}`);
}

function repository(opts, directory) {
  return { type: 'git', url: `git+https://github.com/${opts.repository}.git`, directory };
}

function copyDir(from, to) {
  fs.mkdirSync(to, { recursive: true });
  for (const e of fs.readdirSync(from, { withFileTypes: true })) {
    const s = path.join(from, e.name);
    const d = path.join(to, e.name);
    if (e.isDirectory()) copyDir(s, d);
    else fs.copyFileSync(s, d);
  }
}

function buildPlatformPackage(opts, tool, triple, src, version) {
  const t = TARGETS[triple];
  const name = `@ts-r.dev/${tool.id}-${t.suffix}`;
  const exe = `${tool.binary}${t.exe || ''}`;
  const from = path.join(src, exe);
  if (!fs.existsSync(from)) die(`${from} not found; build ${tool.binary} for ${triple} first`);
  const dest = path.join(opts.out, 'platforms', `${tool.id}-${t.suffix}`);
  fs.mkdirSync(path.join(dest, 'bin'), { recursive: true });
  fs.copyFileSync(from, path.join(dest, 'bin', exe));
  fs.chmodSync(path.join(dest, 'bin', exe), 0o755);
  copyLicense(dest);
  const files = ['bin', 'README.md', 'LICENSE'];
  if (tool.libs) {
    // The binary looks for lib.*.d.ts in <exe dir>/../lib (ts_r_checker::LibStore::locate).
    const libs = fs.readdirSync(opts.libDir).filter((f) => /^lib\..*\.d\.ts$/.test(f)).sort();
    if (!libs.includes('lib.es5.d.ts')) die(`${opts.libDir} has no lib.es5.d.ts; run cargo xtask upstream sync --minimal`);
    fs.mkdirSync(path.join(dest, 'lib'), { recursive: true });
    for (const f of libs) fs.copyFileSync(path.join(opts.libDir, f), path.join(dest, 'lib', f));
    // TypeScript's lib files are Apache-2.0: ship its license and notices with them.
    const upstream = path.resolve(opts.libDir, '../../../..');
    for (const f of ['LICENSE.txt', 'ThirdPartyNoticeText.txt']) {
      const p = path.join(upstream, f);
      if (!fs.existsSync(p)) die(`${p} not found (needed next to TypeScript's lib files); run cargo xtask upstream sync --minimal`);
      fs.copyFileSync(p, path.join(dest, 'lib', `TypeScript-${f}`));
    }
    files.push('lib');
  }
  const pkg = {
    name,
    version,
    description: `The ${tool.binary} binary for ${t.suffix}. Installed automatically by ${tool.front}; don't depend on it directly.`,
    license: 'MIT',
    repository: repository(opts, 'npm'),
    os: [t.os],
    cpu: [t.cpu],
    ...(t.libc ? { libc: [t.libc] } : {}),
    files,
    preferUnplugged: true,
  };
  writeJson(path.join(dest, 'package.json'), pkg);
  fs.writeFileSync(
    path.join(dest, 'README.md'),
    `# ${name}\n\nThe \`${tool.binary}\` binary for ${t.suffix} (Rust target \`${triple}\`), part of [ts-r](https://www.npmjs.com/package/@ts-r.dev/cli).\nInstalled automatically as an optional dependency of \`${tool.front}\`; depend on that instead.\n` +
      (tool.libs ? `\n\`lib/\` holds TypeScript's \`lib.*.d.ts\` files (Apache-2.0, see \`lib/TypeScript-LICENSE.txt\`).\n` : ''),
  );
  return { name, dir: dest, kind: 'platform' };
}

function frontPackageJson(opts, tool, version) {
  return {
    name: tool.front,
    version,
    description: `ts-r ${tool.about}. Prebuilt native binary, no install scripts.`,
    license: 'MIT',
    repository: repository(opts, 'npm'),
    keywords: ['typescript', 'ts-r', tool.id, 'rust'],
    bin: { [tool.binary]: `bin/${tool.binary}.js` },
    files: [`bin/${tool.binary}.js`, 'README.md', 'LICENSE', ...(tool.matcher ? [MATCHER] : [])],
    engines: { node: '>=18' },
    optionalDependencies: Object.fromEntries(Object.values(TARGETS).map((t) => [`@ts-r.dev/${tool.id}-${t.suffix}`, version])),
    'ts-r': { binary: tool.binary, platformPrefix: `@ts-r.dev/${tool.id}-`, ...(tool.version === false ? { version: false } : {}) },
  };
}

function writeShim(dest, tool) {
  fs.mkdirSync(path.join(dest, 'bin'), { recursive: true });
  const to = path.join(dest, 'bin', `${tool.binary}.js`);
  fs.copyFileSync(path.join(npmDir, 'ts-r', 'bin', 'ts-r.js'), to);
  fs.chmodSync(to, 0o755);
}

// The GitHub Actions problem matcher for tsc-style output with TS and TSR codes (record 0007,
// "Decisions", question 2), shipped in `ts-r` and in the tools that print TSR codes.
const MATCHER = 'problem-matcher.json';

function copyMatcher(dest) {
  fs.copyFileSync(path.join(npmDir, 'ts-r', MATCHER), path.join(dest, MATCHER));
}

function buildToolFront(opts, tool, version) {
  const dest = path.join(opts.out, 'fronts', tool.id);
  writeShim(dest, tool);
  copyLicense(dest);
  if (tool.matcher) copyMatcher(dest);
  writeJson(path.join(dest, 'package.json'), frontPackageJson(opts, tool, version));
  fs.writeFileSync(
    path.join(dest, 'README.md'),
    `# ${tool.front}\n\n\`${tool.binary}\`: ${tool.about}, from [ts-r](https://www.npmjs.com/package/@ts-r.dev/cli).\n\n` +
      '```sh\n' +
      `npm i -D ${tool.front}\nnpx ${tool.binary} --help\n` +
      '```\n\n' +
      `Installs only \`${tool.binary}\`: npm picks the one \`@ts-r.dev/${tool.id}-<platform>\` optional dependency that matches\n` +
      'your OS, CPU and libc. The `bin` is a small Node shim that runs the native binary with your arguments and\n' +
      'exits with its exit code. `TS_R_BIN_DIR=<dir>` makes it run the binaries in `<dir>` instead.\n' +
      (tool.matcher
        ? `\nIn GitHub Actions, \`echo "::add-matcher::node_modules/${tool.front}/${MATCHER}"\` before the run turns its\n` +
          '`TS` and sound-mode `TSR` errors into annotations.\n'
        : ''),
  );
  return { name: tool.front, dir: dest, kind: 'front' };
}

function buildLauncher(opts, version) {
  const tool = TOOLS.find((t) => t.id === 'cli');
  const dest = path.join(opts.out, 'fronts', 'ts-r');
  writeShim(dest, tool);
  const pkg = readJson(path.join(npmDir, 'ts-r', 'package.json'));
  pkg.version = version;
  pkg.repository = repository(opts, 'npm/ts-r');
  pkg.optionalDependencies = Object.fromEntries(Object.values(TARGETS).map((t) => [`@ts-r.dev/cli-${t.suffix}`, version]));
  pkg.dependencies = Object.fromEntries(TOOLS.filter((t) => t.id !== 'cli').map((t) => [t.front, version]));
  pkg['ts-r'] = { binary: 'ts-r', platformPrefix: '@ts-r.dev/cli-', tools: TOOLS.filter((t) => t.id !== 'cli').map((t) => t.front) };
  writeJson(path.join(dest, 'package.json'), pkg);
  fs.copyFileSync(path.join(npmDir, 'ts-r', 'README.md'), path.join(dest, 'README.md'));
  copyMatcher(dest);
  copyLicense(dest);
  return { name: pkg.name, dir: dest, kind: 'front' };
}

function buildDropIn(opts, version) {
  const src = path.join(npmDir, 'typescript');
  const dest = path.join(opts.out, 'fronts', 'typescript');
  for (const d of ['bin', 'lib']) copyDir(path.join(src, d), path.join(dest, d));
  fs.chmodSync(path.join(dest, 'bin', 'tsc'), 0o755);
  fs.copyFileSync(path.join(src, 'README.md'), path.join(dest, 'README.md'));
  copyLicense(dest);
  const pkg = readJson(path.join(src, 'package.json'));
  pkg.version = dropInVersion(pkg['ts-r'].typescript, version);
  pkg.repository = repository(opts, 'npm/typescript');
  pkg.dependencies['@ts-r.dev/check'] = version;
  pkg['ts-r'].version = version;
  writeJson(path.join(dest, 'package.json'), pkg);
  return { name: pkg.name, dir: dest, kind: 'front', version: pkg.version };
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  checkConsistency();
  const host = hostTarget();
  const version = opts.version || workspaceVersion();
  const targets = opts.targets.length ? opts.targets : [host];
  for (const t of targets) if (!TARGETS[t]) die(`unsupported target ${t}; supported: ${Object.keys(TARGETS).join(', ')}`);
  fs.rmSync(opts.out, { recursive: true, force: true });
  fs.mkdirSync(opts.out, { recursive: true });

  const packages = [];
  for (const triple of targets) {
    const src = binaryDir(opts, triple, host);
    for (const tool of TOOLS) packages.push(buildPlatformPackage(opts, tool, triple, src, version));
  }
  for (const tool of TOOLS.filter((t) => t.id !== 'cli')) packages.push(buildToolFront(opts, tool, version));
  packages.push(buildLauncher(opts, version));
  packages.push(buildDropIn(opts, version));

  const list = packages.map((p) => ({ name: p.name, version: p.version || version, kind: p.kind, dir: path.relative(opts.out, p.dir).split(path.sep).join('/') }));
  writeJson(path.join(opts.out, 'packages.json'), { version, targets, packages: list });
  for (const p of list) console.log(`${p.name}@${p.version}  ${p.dir}`);
  console.log(`\n${list.length} packages in ${opts.out}; publish in packages.json order (platforms first).`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
