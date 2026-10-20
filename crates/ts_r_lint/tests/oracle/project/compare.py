"""Compare ESLint (per-package JSON) with ts-r-lint (one JSON) on the private-app copy.

Usage: python3 -I compare.py <copy> <eslint dir> <ts-r-lint json> <rules.txt>
Only rules listed in rules.txt (ts-r-lint's built-in rules) are compared, and only files
inside a package that has an eslint.config.js.
"""
import collections
import glob
import json
import os
import sys

copy, eslint_dir, tsr_json, rules_txt = sys.argv[1:5]
copy = os.path.realpath(copy)
ours = {l.split()[0] for l in open(rules_txt) if l.strip() and not l.startswith(" ")}


def rel(p):
    return os.path.relpath(os.path.realpath(p), copy)


pkgs = set()
expected = collections.Counter()
files = set()
for f in glob.glob(os.path.join(eslint_dir, "*.json")):
    pkgs.add(os.path.basename(f)[:-5].replace("__", "/"))
    for r in json.load(open(f)):
        files.add(rel(r["filePath"]))
        for m in r["messages"]:
            if m.get("ruleId") in ours:
                expected[(rel(r["filePath"]), m["ruleId"], m["line"], m["column"])] += 1

actual = collections.Counter()
tsr_files = set()
for r in json.load(open(tsr_json)):
    p = rel(r["filePath"])
    if "ALL" not in pkgs and not any(p.startswith(k + "/") for k in pkgs):
        continue
    tsr_files.add(p)
    for m in r["messages"]:
        if m.get("ruleId") in ours:
            actual[(p, m["ruleId"], m["line"], m["column"])] += 1

per = collections.defaultdict(lambda: [0, 0, 0])
for k in set(expected) | set(actual):
    e, a = expected[k], actual[k]
    per[k[1]][0] += min(e, a)
    per[k[1]][1] += max(0, e - a)
    per[k[1]][2] += max(0, a - e)
    if e != a:
        print("DIFF", k, "eslint", e, "ts-r-lint", a)
print(f"files: eslint {len(files)}, ts-r-lint {len(tsr_files)}")
only_e = sorted(files - tsr_files)
only_t = sorted(tsr_files - files)
print("files only linted by eslint:", only_e[:10], len(only_e))
print("files only linted by ts-r-lint:", only_t[:10], len(only_t))
tot = [0, 0, 0]
for rule in sorted(per):
    m, mi, ex = per[rule]
    tot = [tot[0] + m, tot[1] + mi, tot[2] + ex]
    print(f"{rule:50} match {m:4} miss {mi:4} extra {ex:4}")
print(f"{'TOTAL':50} match {tot[0]:4} miss {tot[1]:4} extra {tot[2]:4}")
