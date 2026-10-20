#!/bin/bash
# parse<T> against typia on the ten API types of tests/fixtures/app/types.ts (record 0007,
# M6 speed gate). Installs typia and TypeScript into a temporary directory (deleted at the
# end unless KEEP=1), generates typia's validators with its own CLI, bundles both sides with
# ts-r-build (--bundle --minify, and unminified), prints bundle sizes and runs bench.mjs
# (throughput per type and operation, geometric means). Everything runs under
# scripts/limit.sh. Needs Node and network access for npm.
#
#   cargo build --release -p ts_r_build && crates/ts_r_guard/bench/typia/run.sh
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
LIMIT=$ROOT/scripts/limit.sh
TSR=$ROOT/target/release/ts-r-build
APP=$ROOT/crates/ts_r_guard/tests/fixtures/app
TYPIA_VERSION=${TYPIA_VERSION:-9.7.2}
TS_VERSION=${TS_VERSION:-5.8.3}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/ts-r-guard-typia.XXXXXX")
cleanup() { [ "${KEEP:-0}" = 1 ] && echo "kept $WORK" || rm -rf "$WORK"; }
trap cleanup EXIT

# Downloaded packages in their own directory; the bench sources in another.
mkdir -p "$WORK/deps" "$WORK/src/typia-src" "$WORK/src/out"
echo '{ "name": "deps", "private": true }' > "$WORK/deps/package.json"
(cd "$WORK/deps" && "$LIMIT" --time-s 300 -- npm install --no-audit --no-fund --ignore-scripts \
  "typia@$TYPIA_VERSION" "typescript@$TS_VERSION" >/dev/null)
S=$WORK/src
cp "$HERE"/*.ts "$HERE/bench.mjs" "$S/"
cp "$HERE"/typia-src/*.ts "$S/typia-src/"
cp "$APP/types.ts" "$S/types.ts"
cp "$APP/types.ts" "$S/typia-src/types.ts"
cp "$APP/size-all.ts" "$S/size-parse.ts"
cp -R "$APP/node_modules/@ts-r.dev" "$WORK/deps/node_modules/"
ln -s "$WORK/deps/node_modules" "$S/node_modules"
echo '{ "name": "guard-bench", "private": true, "type": "module" }' > "$S/package.json"
echo '{ "compilerOptions": { "strict": true, "target": "es2022", "module": "esnext", "moduleResolution": "bundler", "noEmit": true, "skipLibCheck": true }, "include": ["*.ts", "typia-src/*.ts"] }' > "$S/tsconfig.json"

cd "$S"
"$LIMIT" --time-s 300 -- node "$WORK/deps/node_modules/typia/lib/executable/typia.js" generate \
  --input typia-src --output typia-gen --project tsconfig.json >/dev/null
build() { "$LIMIT" --time-s 120 -- "$TSR" "$1" --bundle --format=esm --no-cache "--outfile=$2" "${@:3}" >/dev/null 2>&1; }
for f in ours data size-parse size-is; do build $f.ts out/$f.js --minify; done
for f in theirs size-assert size-is; do build typia-gen/$f.ts out/typia-$f.js --minify; done
# Unminified too: ts-r-build's minifier turns module-level `const` into `let`, which V8 does
# not constant-fold (about 20% on nested validators; typia's are function-local).
build ours.ts out/ours-plain.js
build typia-gen/theirs.ts out/typia-theirs-plain.js

echo "bundle sizes (bytes, minified / gzip -9):"
for f in size-parse typia-size-assert size-is typia-size-is ours typia-theirs; do
  printf '  %-18s %6d / %5d\n' "$f" "$(wc -c < out/$f.js)" "$(gzip -9c out/$f.js | wc -c)"
done
echo "== minified"
"$LIMIT" --mem-mb 2048 --time-s 300 -- node bench.mjs out/ours.js out/typia-theirs.js out/data.js
echo "== unminified"
"$LIMIT" --mem-mb 2048 --time-s 300 -- node bench.mjs out/ours-plain.js out/typia-theirs-plain.js out/data.js
