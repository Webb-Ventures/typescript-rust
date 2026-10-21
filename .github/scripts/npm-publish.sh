#!/usr/bin/env bash
# npm-publish.sh <npm-dist dir>: publishes the packages build-packages.mjs wrote, in two parallel
# phases: every platform package at once, then every front package (the tool packages, the
# @ts-r.dev/cli launcher and @ts-r.dev/typescript) at once. A front only goes out once all the
# platform packages it points at are on npm, so a failed platform phase stops the run.
#
# Env: NPM_TAG (latest or next), DRY_RUN (true: npm publish --dry-run), JOBS (parallel
# publishes, default 16). Versions already on npm are skipped, so a re-run publishes only what
# is missing.
set -euo pipefail

dist=$1
export DIST=$dist
export JOBS=${JOBS:-16}

# One package: <name> <version> <dir>. Output is prefixed with the name (the phases interleave).
publish_one() {
  local name=$1 version=$2 dir=$3
  local flags=(--provenance --access public --tag "$NPM_TAG")
  if [ "$DRY_RUN" = true ]; then
    flags+=(--dry-run)
  elif npm view "$name@$version" version >/dev/null 2>&1; then
    echo "[$name] $version is already published; skipping"
    return 0
  fi
  # A second try covers a registry hiccup (rate limit, timeout).
  for attempt in 1 2; do
    if npm publish "$DIST/$dir" "${flags[@]}" 2>&1 | sed "s|^|[$name] |"; then
      return 0
    fi
    # npm may have accepted it before the error.
    if [ "$DRY_RUN" != true ] && npm view "$name@$version" version >/dev/null 2>&1; then
      return 0
    fi
    [ "$attempt" = 1 ] && sleep 15
  done
  echo "::error::[$name] npm publish failed"
  return 1
}
export -f publish_one

phase() { # <kind>
  local list
  list=$(node -e '
    const kind = process.argv[2];
    for (const p of require(process.argv[1]).packages) if (p.kind === kind) console.log([p.name, p.version, p.dir].join(" "));
  ' "$(cd "$DIST" && pwd)/packages.json" "$1")
  local count
  count=$(printf '%s\n' "$list" | grep -c . || true)
  echo "::group::$1 packages ($count, $JOBS at a time)"
  local status=0
  printf '%s\n' "$list" | grep . | xargs -P "$JOBS" -L 1 bash -c 'set -o pipefail; publish_one "$@"' _ || status=$?
  echo "::endgroup::"
  if [ "$status" != 0 ]; then
    echo "::error::some $1 packages failed to publish (above); stopping"
    exit 1
  fi
}

phase platform
phase front
