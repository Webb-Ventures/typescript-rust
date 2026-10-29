#!/bin/sh
# Rebuilds and restarts the review box from a staged copy of this plugin: what install.sh does on
# an update, run by the dispatcher in a container of its own (named ts-r-deploy, not tsr-*, so the
# dispatcher it restarts leaves it alone) from the image already running, so a broken deploy.sh on
# main cannot stop the next deploy. See the README's "Deploys".
#   deploy.sh <staged plugin directory> <commit>
# The build runs first: if it fails, nothing is restarted. If the new dispatcher does not answer
# /healthz within three minutes, the image from before (tagged ts-r-review:previous) comes back.
set -eu
src=$1
sha=$2
DATA=/srv/ts-r-review
mkdir -p "$DATA/deploy"
exec >>"$DATA/logs/deploy-$(printf %.7s "$sha").log" 2>&1
echo "=== $(date -u +%FT%TZ) deploying $sha from $src"

# The compose file of what is running now, for a rollback: the last deploy's, else the new one.
current=$(cat "$DATA/deploy/current" 2>/dev/null || true)
previous_src=$src
if [ -n "$current" ] && [ -f "$DATA/deploy/$current/.agents/plugins/code-review/box/compose.yaml" ]; then
  previous_src=$DATA/deploy/$current/.agents/plugins/code-review
fi

docker tag ts-r-review:latest ts-r-review:previous
docker compose -f "$src/box/compose.yaml" build
image=$(docker image inspect --format '{{.Id}}' ts-r-review:latest)

# The new dispatcher must be the one running, and answer twice, ten seconds apart (it exits a
# minute after starting when a secret is missing, and restarts).
up() {
  [ "$(docker inspect --format '{{.Image}} {{.State.Running}}' ts-r-review-dispatcher-1 2>/dev/null)" = "$image true" ] &&
    curl -fsS --max-time 5 -o /dev/null http://127.0.0.1:8787/healthz
}
healthy=
if docker compose -f "$src/box/compose.yaml" up -d --remove-orphans; then
  for _ in $(seq 36); do
    if up && sleep 10 && up; then healthy=1; break; fi
    sleep 5
  done
fi
if [ -z "$healthy" ]; then
  echo "=== the new dispatcher did not come up: rolling back"
  docker logs --tail 50 ts-r-review-dispatcher-1 || true
  docker tag ts-r-review:previous ts-r-review:latest
  docker compose -f "$previous_src/box/compose.yaml" up -d --no-build --remove-orphans
  exit 1
fi

printf '%s\n' "$sha" >"$DATA/deploy/current.new"
mv "$DATA/deploy/current.new" "$DATA/deploy/current"
docker image prune -f --filter label=com.docker.compose.project=ts-r-review >/dev/null || true
echo "=== $(date -u +%FT%TZ) deployed $sha"
