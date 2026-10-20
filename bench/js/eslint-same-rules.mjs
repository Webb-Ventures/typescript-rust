// The setup step of the `lint-*-same-rules` scenarios: writes ESLint flat configs that are the
// repo's own config with every rule ts-r-lint does not implement removed, so ESLint and
// ts-r-lint run the same rules, with the same options, on the same files. The rule list comes
// from the ts-r-lint binary under test (`--list-rules`, its rule registry), so the configs stay
// in step with the rules that binary has. Everything else in the repo's config (files, ignores,
// plugins, parser options, linter options) is kept.
//
//   node bench/js/eslint-same-rules.mjs <ts-r-lint> <repo config> <out prefix>
//
// writes two configs next to the repo config:
//
//   <out prefix>.config.mjs          every configured rule ts-r-lint implements, type-aware
//                                    ones included (type information as the repo configures it)
//   <out prefix>-untyped.config.mjs  only the ones that need no type information, and
//                                    `parserOptions.project`/`projectService` off, so ESLint
//                                    builds no TypeScript program (ts-r-lint: --no-type-check)
//
// A rule needs type information when its plugin says so (`meta.docs.requiresTypeChecking`,
// as typescript-eslint's rules do); ESLint's core rules never do. Paths are relative to the
// working directory (the checkout); the configs sit next to the repo config so that both tools
// resolve its `files` and `ignores` patterns from the same directory. Prints the kept and
// dropped rules on stderr.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [bin, baseArg, prefixArg] = process.argv.slice(2);
if (!bin || !baseArg || !prefixArg) {
  console.error('usage: node eslint-same-rules.mjs <ts-r-lint> <repo config> <out prefix>');
  process.exit(2);
}
const base = path.resolve(baseArg);
const prefix = path.resolve(prefixArg);
if (path.dirname(base) !== path.dirname(prefix)) {
  console.error('eslint-same-rules: the output configs must be in the same directory as the repo config');
  process.exit(2);
}

const implemented = new Set(
  execFileSync(path.resolve(bin), ['--list-rules'], { encoding: 'utf8' })
    .split('\n')
    .map(l => l.trim())
    .filter(Boolean),
);
if (implemented.size === 0) {
  console.error(`eslint-same-rules: ${bin} --list-rules printed no rules`);
  process.exit(1);
}

// Every rule the repo's config names (on or off), and which of them need type information.
const configs = [await (await import(pathToFileURL(base).href)).default].flat(Infinity);
const plugins = {};
for (const c of configs) Object.assign(plugins, c?.plugins ?? {});
const configured = new Set(configs.flatMap(c => Object.keys(c?.rules ?? {})));
const needsTypes = name => {
  const slash = name.lastIndexOf('/');
  if (slash < 0) return false;
  const rule = plugins[name.slice(0, slash)]?.rules?.[name.slice(slash + 1)];
  return Boolean(rule?.meta?.docs?.requiresTypeChecking);
};
const sorted = s => [...s].sort();
const typed = sorted(configured).filter(n => implemented.has(n));
const untyped = typed.filter(n => !needsTypes(n));
const dropped = sorted(configured).filter(n => !implemented.has(n));

const write = (file, keep, untypedOnly) => {
  const text = `// Written by bench/js/eslint-same-rules.mjs (cargo xtask bench, scenario setup); do not edit.
// ${path.basename(base)} with only the configured rules ${path.basename(bin)} implements${untypedOnly ? ' that need no type information,\n// and no TypeScript program' : ''}.
import base from './${path.basename(base)}';

const KEEP = new Set(${JSON.stringify(keep, null, 2)});

export default [
  ...[await base].flat(Infinity).map(c =>
    c && c.rules
      ? { ...c, rules: Object.fromEntries(Object.entries(c.rules).filter(([name]) => KEEP.has(name))) }
      : c,
  ),${untypedOnly ? '\n  { languageOptions: { parserOptions: { project: false, projectService: false } } },' : ''}
];
`;
  // Rewrite only when it changed, so ts-r-lint's config cache stays valid between runs.
  if (!fs.existsSync(file) || fs.readFileSync(file, 'utf8') !== text) fs.writeFileSync(file, text);
};
write(`${prefix}.config.mjs`, typed, false);
write(`${prefix}-untyped.config.mjs`, untyped, true);

console.error(
  `eslint-same-rules: ${configured.size} configured rules; ${typed.length} implemented by ts-r-lint ` +
    `(${typed.length - untyped.length} of them type-aware); dropped ${dropped.length}: ${dropped.join(', ')}`,
);
