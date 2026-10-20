"""Prepare a scratch copy of a pnpm monorepo for the project lint oracle.

Usage: python3 -I setup.py <real repo> <copy dir> <dir with eslint, typescript-eslint, globals>

Copies the sources (no node_modules, .git or .terraform), links the real root node_modules
entries plus the lint tooling into the copy, maps the shared config package in
`tooling/eslint` (private-app's, under the name in its package.json) into node_modules, and stubs plugins that are not installed
(eslint-plugin-react-hooks, @next/eslint-plugin-next) so every config loads in ESLint and in
ts-r-lint. The real repo is never written to.
"""
import json
import os
import re
import shutil
import sys

real, copy, tooling_nm = sys.argv[1:4]
shutil.copytree(
    real,
    copy,
    ignore=shutil.ignore_patterns("node_modules", ".git", ".terraform", "*.tfstate*"),
    dirs_exist_ok=True,
)
nm = os.path.join(copy, "node_modules")
os.makedirs(nm, exist_ok=True)
links = [(os.path.join(real, "node_modules", e), e) for e in os.listdir(os.path.join(real, "node_modules"))]
links += [(os.path.join(tooling_nm, e), e) for e in ("typescript-eslint", "globals", "eslint", "@eslint", "typescript")]
for src, name in links:
    dst = os.path.join(nm, name)
    if os.path.exists(src) and not os.path.lexists(dst):
        os.symlink(src, dst)
shared = os.path.join(copy, "tooling", "eslint")
if os.path.isdir(shared):
    tnm = os.path.join(shared, "node_modules")
    if not os.path.lexists(tnm):
        os.symlink(tooling_nm, tnm)
    with open(os.path.join(shared, "package.json")) as f:
        name = json.load(f)["name"]
    dst = os.path.join(nm, *name.split("/"))
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if not os.path.lexists(dst):
        os.symlink(shared, dst)
    idx = os.path.join(shared, "index.js")
    s = open(idx).read()
    s = s.replace(
        "import reactHooks from 'eslint-plugin-react-hooks'",
        "const reactHooks = { configs: { flat: { 'recommended-latest': {} } } }",
    )
    open(idx, "w").write(s)
for root, dirs, files in os.walk(copy):
    dirs[:] = [d for d in dirs if d != "node_modules"]
    if "eslint.config.js" in files:
        p = os.path.join(root, "eslint.config.js")
        s = open(p).read()
        s2 = re.sub(r"import next from '@next/eslint-plugin-next'\n", "", s)
        s2 = re.sub(r"\n\s*next\.configs\['core-web-vitals'\],", "", s2)
        if s2 != s:
            open(p, "w").write(s2)
