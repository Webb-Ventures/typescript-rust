"""Write an all-rules oracle config into the private-app copy.

Usage: python3 -I all_rules_config.py <copy> <rules.txt> <real tooling/eslint/node_modules>
"""
import json
import os
import sys

copy, rules_txt, tooling_nm = sys.argv[1:4]
rules = [l.split()[0] for l in open(rules_txt) if l.strip() and not l.startswith(" ")]
nm = os.path.join(copy, "node_modules")
for pkg in ("typescript-eslint", "globals", "eslint", "@eslint", "typescript"):
    dst = os.path.join(nm, pkg)
    if not os.path.lexists(dst):
        os.symlink(os.path.join(tooling_nm, pkg), dst)
cfg = """// ts-r-lint oracle: every built-in ts-r-lint rule at its default options.
import tseslint from 'typescript-eslint'
import globals from 'globals'

const rules = Object.fromEntries(%s.map((r) => [r, 'error']))

export default [
  {
    ignores: ['**/dist/**', '**/.next/**', '**/.wrangler/**', '**/coverage/**', '**/generated/**', '**/*.gen.ts'],
  },
  {
    files: ['**/*.{js,mjs,cjs,jsx,ts,tsx,mts,cts}'],
    plugins: { '@typescript-eslint': tseslint.plugin },
    languageOptions: {
      parser: tseslint.parser,
      globals: { ...globals.node, ...globals.browser },
    },
    linterOptions: { reportUnusedDisableDirectives: 'off' },
    rules,
  },
]
""" % json.dumps(rules)
open(os.path.join(copy, "oracle.config.mjs"), "w").write(cfg)
print(len(rules), "rules")
