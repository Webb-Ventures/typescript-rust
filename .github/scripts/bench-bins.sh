#!/usr/bin/env bash
# bench-bins.sh <release-dir> <out-dir>: copies the ts-r executables of a release build (the
# binaries cargo xtask bench runs with --bin-dir) into <out-dir>, so a later build into the same
# target directory can't overwrite them.
set -euo pipefail
from=$1
out=$2
mkdir -p "$out"
n=0
for f in "$from"/ts-r "$from"/ts-r-* "$from"/ts-r.exe "$from"/ts-r-*.exe; do
  case "$f" in *.d | *.pdb) continue ;; esac
  if [ -f "$f" ] && [ -x "$f" ]; then
    cp "$f" "$out/"
    n=$((n + 1))
  fi
done
if [ "$n" = 0 ]; then
  echo "::error::no ts-r executables in $from" >&2
  exit 1
fi
echo "copied $n executables to $out"
