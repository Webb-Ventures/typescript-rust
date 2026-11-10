#!/usr/bin/env bash
# npm-next-tag.sh [--apply]: points the `next` dist-tag of every ts-r npm package at the version
# its `latest` tag names, when `latest` is a prerelease and `next` lags behind it.
#
# Why: while no package has a stable version, the release workflow publishes each prerelease
# under `latest` and then adds `next` (.github/scripts/npm-publish.sh). That second step needs the
# trusted publisher's "Allow npm dist-tag" permission, which is a setting on npmjs.com only
# (npm 12.2's `npm trust` sets --allow-publish and --allow-stage-publish, nothing for dist-tags).
# Without it the step fails with a warning and `next` stays behind. Run this with your own npm
# login to catch `next` up.
#
# Without --apply it only prints what it would change (reads the registry, changes nothing).
# With --apply, npm asks for two-factor authentication on the first change: choose "skip
# two-factor authentication for the next 5 minutes" on the npm website and the rest go through
# (npm's bulk-usage advice for `npm trust`: one call every 2 seconds, about 80 in that window).
# Run it in a terminal: npm prints a URL (or opens the browser) and waits while you sign in.
# With an authenticator app instead, pass --otp=<code> (it lasts about 30 seconds, so prefer
# the browser's 5-minute skip). The first failure stops the run. Re-running is safe: packages
# whose `next` already matches are skipped.
#
# Or with a token: NPM_TOKEN=<token> scripts/npm-next-tag.sh --apply. Use a granular access
# token (npmjs.com > Access Tokens): packages and scopes read and write, scope @ts-r.dev,
# "Bypass two-factor authentication" on, a short expiry; revoke it afterwards. npm refuses it on
# a package whose Publishing access is "Require two-factor authentication and disallow tokens".
# The token is read from the environment for this run only and never written to disk.
set -euo pipefail

if [ -n "${NPM_TOKEN:-}" ]; then
  # npm expands ${NPM_TOKEN} in an .npmrc itself, so the file holds the placeholder, not the token.
  npmrc=$(mktemp)
  trap 'rm -f "$npmrc"' EXIT
  # shellcheck disable=SC2016 # literal ${NPM_TOKEN}: npm expands it, not the shell
  printf '//registry.npmjs.org/:_authToken=${NPM_TOKEN}\n' > "$npmrc"
  export NPM_CONFIG_USERCONFIG="$npmrc"
  echo "Using NPM_TOKEN from the environment (as $(npm whoami 2>/dev/null || echo 'an unknown user: is the token valid?'))"
fi

apply=false otp=()
for arg in "$@"; do
  case "$arg" in
    --apply) apply=true ;;
    --otp=?*) otp=("$arg") ;;
    *) echo "usage: scripts/npm-next-tag.sh [--apply [--otp=<code>]]" >&2; exit 2 ;;
  esac
done

# The 64 packages in npm/README.md: the `ts-r` launcher, the drop-in, six tools, and each of the
# seven binaries for eight platforms.
tools=(cli run dev build check lint lsp)
platforms=(darwin-arm64 darwin-x64 linux-arm64-gnu linux-arm64-musl linux-x64-gnu linux-x64-musl win32-arm64-msvc win32-x64-msvc)
packages=()
for t in "${tools[@]}"; do
  packages+=("@ts-r.dev/$t")
  for p in "${platforms[@]}"; do packages+=("@ts-r.dev/$t-$p"); done
done
packages+=("@ts-r.dev/typescript")

changed=0 skipped=0 failed=0
for name in "${packages[@]}"; do
  if ! tags=$(npm view "$name" dist-tags --json 2>/dev/null); then
    echo "$name: not on npm (or the registry didn't answer); skipped"
    failed=$((failed + 1)); continue
  fi
  # Prints "<latest> <next> <behind>": behind is yes when `next` names an older version than
  # `latest` (or none). Semver precedence, the same rule as tag_is_newer in
  # .github/scripts/npm-publish.sh: a release is newer than its prereleases, numeric
  # identifiers compare as numbers.
  # shellcheck disable=SC2016 # JavaScript template literal, not shell
  read -r latest next behind <<<"$(node -e '
    const t = JSON.parse(process.argv[1] || "{}");
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
    const behind = t.latest && (!t.next || cmp(t.next, t.latest) < 0);
    console.log(`${t.latest || "-"} ${t.next || "-"} ${behind ? "yes" : "no"}`);
  ' "$tags")"
  if [ "$latest" = - ] || [[ "$latest" != *-* ]]; then
    echo "$name: latest is ${latest} (not a prerelease); left alone"
    skipped=$((skipped + 1)); continue
  fi
  if [ "$behind" != yes ]; then
    # `next` already names `latest` or something newer (a publish under `next` only): keep it.
    [ "$next" = "$latest" ] || echo "$name: next $next is newer than latest $latest; left alone"
    skipped=$((skipped + 1)); continue
  fi
  if [ "$apply" = true ]; then
    echo "$name: next $next -> $latest"
    # npm's output stays on the terminal: npm only offers its browser two-factor flow when
    # stdout is a terminal, and fails with EOTP otherwise.
    if npm dist-tag add "$name@$latest" next ${otp[@]+"${otp[@]}"}; then
      changed=$((changed + 1))
    else
      # One failure (usually 2FA) would repeat for every package: stop here instead.
      echo "$name: npm dist-tag add failed; stopping after $changed moved. Re-run to continue." >&2
      exit 1
    fi
    sleep 2
  else
    echo "$name: would move next $next -> $latest"
    changed=$((changed + 1))
  fi
done

verb=would\ move; [ "$apply" = true ] && verb=moved
echo "${#packages[@]} packages: $verb $changed, $skipped already right or left alone, $failed failed"
[ "$apply" = true ] || [ "$changed" = 0 ] || echo "Run again with --apply to change them."
[ "$failed" = 0 ]
