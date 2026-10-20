"""Median wall time of a command.

Usage: python3 -I bench.py <runs> <cwd> -- cmd args...
"""
import statistics
import subprocess
import sys
import time

runs = int(sys.argv[1])
cwd = sys.argv[2]
cmd = sys.argv[sys.argv.index("--") + 1:]
times = []
for _ in range(runs):
    t = time.time()
    subprocess.run(cmd, cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    times.append(time.time() - t)
print(f"median {statistics.median(times):.3f}s  min {min(times):.3f}s  runs {runs}  {' '.join(cmd)[-90:]}")
