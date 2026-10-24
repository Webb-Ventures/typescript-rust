#!/usr/bin/env bash
# npm-publish.sh <npm-dist dir>: publishes the packages build-packages.mjs wrote, in two parallel
# phases: every platform package at once, then every front package (the tool packages, the
# @ts-r.dev/cli launcher and @ts-r.dev/typescript) at once. A front only goes out once all the
# platform packages it points at are on npm, so a failed platform phase stops the run.
#
# Env: NPM_TAG (latest or next), DRY_RUN (true: npm publish --dry-run), JOBS (parallel
# publishes, default 16). Versions already on npm are skipped, so a re-run publishes only what
# is missing.
#
# Readmes: npmjs.com and `npm view <name> readme` show the readme the registry stored for the
# package, and the registry only stores it from a publish that sets the `latest` dist-tag (a
# publish under `next`, or a later `npm dist-tag add`, leaves it alone). So while a package has
# no stable version on npm, a prerelease (NPM_TAG=next) is published under `latest` and then
# tagged `next` as well: both tags name the newest release, as they would anyway with no stable
# one, and the readme is the newest release's. Once a stable version exists, prereleases go
# out under `next` only and `latest` stays on the stable release.
#
# Retries: a dist-tag never moves back. Re-running an older release after a newer one went out
# (say, only its failed npm job) publishes what the older one is missing but leaves every tag
# that already names a newer version where it is.
set -euo pipefail

dist=$1
export DIST=$dist
export JOBS=${JOBS:-16}

# 0 if <name> has a version on npm without a prerelease suffix; 1 if it has none (or isn't on
# npm yet); 2 if npm couldn't be asked.
has_stable() {
  local out
  if ! out=$(npm view "$1" versions --json 2>&1); then
    case "$out" in *E404*) return 1 ;; *) return 2 ;; esac
  fi
  node -e 'process.exit([].concat(JSON.parse(process.argv[1])).some((v) => !v.includes("-")) ? 0 : 1)' "$out"
}
export -f has_stable

# The tag to publish <name> under, and a second tag to add after it (or nothing).
publish_tags() { # <name>
  if [ "$NPM_TAG" = next ]; then
    local status=0
    has_stable "$1" || status=$?
    if [ "$status" = 1 ]; then
      echo "latest next"
      return
    fi
    [ "$status" = 2 ] && echo "::warning::[$1] couldn't list its versions on npm; publishing under next only" >&2
  fi
  echo "$NPM_TAG"
}
export -f publish_tags

# 0 if dist-tag <tag> in <dist-tags JSON> names a version newer than <version> (semver
# precedence: a release is newer than its prereleases, numeric identifiers compare as numbers).
tag_is_newer() { # <dist-tags JSON> <tag> <version>
  node -e '
    const [tags, tag, version] = process.argv.slice(1);
    const current = JSON.parse(tags || "{}")[tag];
    if (!current) process.exit(1);
    const parse = (v) => {
      const i = v.indexOf("-");
      const core = (i < 0 ? v : v.slice(0, i)).split(".").map(Number);
      return [core, i < 0 ? null : v.slice(i + 1).split(".")];
    };
    const ident = (a, b) => {
      const na = /^\d+$/.test(a), nb = /^\d+$/.test(b);
      if (na && nb) return Number(a) - Number(b);
      if (na !== nb) return na ? -1 : 1;
      return a < b ? -1 : a > b ? 1 : 0;
    };
    const cmp = (x, y) => {
      const [cx, px] = parse(x), [cy, py] = parse(y);
      for (let k = 0; k < 3; k++) if (cx[k] !== cy[k]) return cx[k] - cy[k];
      if (!px || !py) return (px ? -1 : 0) - (py ? -1 : 0);
      for (let k = 0; k < Math.min(px.length, py.length); k++) {
        const d = ident(px[k], py[k]);
        if (d) return d;
      }
      return px.length - py.length;
    };
    process.exit(cmp(current, version) > 0 ? 0 : 1);
  ' "$1" "$2" "$3"
}
export -f tag_is_newer

# One package: <name> <version> <dir>. Output is prefixed with the name (the phases interleave).
publish_one() {
  local name=$1 version=$2 dir=$3
  local tag also
  read -r tag also <<<"$(publish_tags "$name")"
  # A retry of an older release keeps tags that already name a newer version. npm publish
  # always sets a tag, so when the main one is taken the version goes out under `retry`, which
  # is removed again afterwards.
  local tags cleanup=
  tags=$(npm view "$name" dist-tags --json 2>/dev/null) || tags='{}'
  if [ -n "$also" ] && tag_is_newer "$tags" "$also" "$version"; then
    echo "[$name] dist-tag $also already names a newer version; leaving it"
    also=
  fi
  if tag_is_newer "$tags" "$tag" "$version"; then
    echo "[$name] dist-tag $tag already names a newer version; leaving it"
    if [ -n "$also" ]; then
      tag=$also also=
    else
      tag=retry cleanup=retry
    fi
  fi
  local flags=(--provenance --access public --tag "$tag")
  echo "[$name] $version: npm dist-tag $tag${also:+, then $also}"
  if [ "$DRY_RUN" = true ]; then
    flags+=(--dry-run)
  elif npm view "$name@$version" version >/dev/null 2>&1; then
    echo "[$name] $version is already published; skipping"
    add_tag "$name" "$version" "$also"
    return 0
  fi
  # A second try covers a registry hiccup (rate limit, timeout).
  for attempt in 1 2; do
    if npm publish "$DIST/$dir" "${flags[@]}" 2>&1 | sed "s|^|[$name] |"; then
      add_tag "$name" "$version" "$also"
      drop_tag "$name" "$cleanup"
      return 0
    fi
    # npm may have accepted it before the error.
    if [ "$DRY_RUN" != true ] && npm view "$name@$version" version >/dev/null 2>&1; then
      add_tag "$name" "$version" "$also"
      drop_tag "$name" "$cleanup"
      return 0
    fi
    [ "$attempt" = 1 ] && sleep 15
  done
  echo "::error::[$name] npm publish failed"
  return 1
}
export -f publish_one

# Points dist-tag <tag> at <name>@<version> (npm 11.21 and newer do this with trusted publishing
# too). A failure is a warning, not an error: the version is out, and a re-run retries the tag.
add_tag() { # <name> <version> <tag or empty>
  [ -n "$3" ] && [ "$DRY_RUN" != true ] || return 0
  npm dist-tag add "$1@$2" "$3" 2>&1 | sed "s|^|[$1] |" && return 0
  echo "::warning::[$1] couldn't point dist-tag $3 at $2: run npm dist-tag add $1@$2 $3"
}
export -f add_tag

# Removes the temporary dist-tag <tag> (empty: nothing to do). A failure is a warning.
drop_tag() { # <name> <tag or empty>
  [ -n "$2" ] && [ "$DRY_RUN" != true ] || return 0
  npm dist-tag rm "$1" "$2" 2>&1 | sed "s|^|[$1] |" && return 0
  echo "::warning::[$1] couldn't remove dist-tag $2: run npm dist-tag rm $1 $2"
}
export -f drop_tag

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
