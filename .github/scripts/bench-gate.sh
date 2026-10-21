#!/usr/bin/env bash
# bench-gate.sh <base-bin-dir> [<repos>]: run from the PR's checkout, with its release build in
# target/release and the base's binaries in <base-bin-dir>. Benches the base (our tools only),
# then the PR (every tool, so the table has the reference tools too) against it, and re-measures
# any flagged scenario with the PR first, so only a regression seen in both orders fails. With
# <repos> (bench/repos.toml names), the pinned repo scenarios run the same way.
#
# BENCH_FILTER (scenario ids, space-separated) limits the run, for trying the script out.
#
# Writes bench/results/comment.md (the tables). Exit 0: no regression; 1: a regression; 2: the
# bench itself failed. The same steps as the review box's bench (code-review plugin, job.mjs).
set -u
base_bin=$1
repos=${2:-}

# shellcheck disable=SC2086 # BENCH_FILTER is a word list.
bench() { cargo xtask bench "$@" ${BENCH_FILTER:-}; }

echo "::group::base (our tools)"
bench --bin-dir "$base_bin" --save base --no-fail --ours-only || exit 2
if [ -n "$repos" ]; then
  bench --repos "$repos" --bin-dir "$base_bin" --save base-repos --no-fail --ours-only || exit 2
fi
echo "::endgroup::"

echo "::group::PR"
bench --save pr --baseline bench/results/base.json --markdown bench/results/comment.md
code=$?
repos_code=0
if [ -n "$repos" ]; then
  bench --repos "$repos" --save pr-repos --baseline bench/results/base-repos.json \
    --markdown bench/results/comment-repos.md
  repos_code=$?
fi
echo "::endgroup::"

confirm() { # <save> [--repos <names>]
  local save=$1
  shift
  echo "::group::re-measuring $save's flagged scenarios, PR first"
  # Word splitting is intended: one argument per scenario id.
  # shellcheck disable=SC2046
  set -- "$@" $(cat "bench/results/$save-regressions.txt")
  bench "$@" --save "$save-confirm" --no-fail --ours-only >/dev/null
  bench "$@" --bin-dir "$base_bin" --save "$save-confirm-base" --no-fail --ours-only >/dev/null
  echo "::endgroup::"
  bench --compare "bench/results/$save-confirm.json" \
    --baseline "bench/results/$save-confirm-base.json" --markdown "bench/results/$save-confirm.md"
}
if [ "$code" = 1 ] && [ -f bench/results/pr-regressions.txt ]; then
  confirm pr
  code=$?
fi
if [ "$repos_code" = 1 ] && [ -f bench/results/pr-repos-regressions.txt ]; then
  confirm pr-repos --repos "$repos"
  repos_code=$?
fi
[ "$code" != 0 ] || code=$repos_code

touch bench/results/comment.md
if [ -f bench/results/comment-repos.md ]; then
  { echo; echo "### Public repos ($repos)"; echo; cat bench/results/comment-repos.md; } >> bench/results/comment.md
fi
for f in pr pr-repos; do
  if [ -f "bench/results/$f-confirm.md" ]; then
    { echo; echo "### Re-run of the flagged scenarios (PR first, then base)"; echo; cat "bench/results/$f-confirm.md"; } >> bench/results/comment.md
  fi
done
case "$code" in 0 | 1) exit "$code" ;; *) exit 2 ;; esac
