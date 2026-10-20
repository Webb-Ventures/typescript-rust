"""Run ESLint in each package of the private-app copy, like `turbo run lint`.

Usage: python3 -I eslint_packages.py <copy> <out dir> <limit.sh> [extra eslint args...]
Writes <out dir>/<pkg>.json per package and prints per-package wall times.
"""
import os
import subprocess
import sys
import time

copy, out, limit = sys.argv[1], sys.argv[2], sys.argv[3]
extra = sys.argv[4:]
eslint = os.path.join(copy, "tooling/eslint/node_modules/eslint/bin/eslint.js")
os.makedirs(out, exist_ok=True)
pkgs = []
for root, dirs, files in os.walk(copy):
    dirs[:] = [d for d in dirs if d not in ("node_modules", ".git")]
    if "eslint.config.js" in files:
        pkgs.append(os.path.relpath(root, copy))
pkgs.sort()
total = 0.0
for p in pkgs:
    name = p.replace("/", "__")
    t = time.time()
    r = subprocess.run(
        [limit, "--mem-mb", "6144", "--", "node", eslint, "-f", "json", "-o",
         os.path.join(out, name + ".json"), *extra, "."],
        cwd=os.path.join(copy, p), capture_output=True, text=True,
    )
    dt = time.time() - t
    total += dt
    err = r.stderr.strip().splitlines()[-1:] if r.returncode not in (0, 1) else []
    print(f"{p:32} {dt:7.2f}s exit={r.returncode} {' '.join(err)[:150]}")
print(f"total {total:.2f}s")
