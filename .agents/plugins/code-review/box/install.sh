#!/usr/bin/env bash
# Sets up or updates the review box. Run it on the box, as the user that owns the deployment
# (uid 1000, in the docker group, with sudo), from a copy of the plugin directory:
#   bash box/install.sh
# Idempotent: re-run after syncing a new version of the plugin to rebuild and restart.
set -euo pipefail
DATA=/srv/ts-r-review
src="$(cd "$(dirname "$0")/.." && pwd)"

if [ "$(id -u)" != 1000 ]; then
  echo "Run as uid 1000: job containers run as node (uid 1000) and must own $DATA." >&2
  exit 1
fi
sudo install -d -o 1000 -g 1000 -m 755 "$DATA"
install -d -m 700 "$DATA/secrets" "$DATA/codex-home"
install -d -m 755 "$DATA/jobs" "$DATA/logs" "$DATA/cache" "$DATA/targets"
if [ ! -f "$DATA/secrets/dispatch-token" ]; then
  (umask 077 && openssl rand -hex 32 > "$DATA/secrets/dispatch-token")
  echo "Created $DATA/secrets/dispatch-token: store it as the CODE_REVIEW_DISPATCH_TOKEN repo secret."
fi
if [ ! -f "$DATA/config.env" ]; then
  cat > "$DATA/config.env" <<'CONFIG'
# The repositories the box serves (the App must be installed on each); requests that name no
# repository are for the first. REPO is the setting from before REPOS, used when REPOS is unset.
REPOS=["Webb-Ventures/tsc-rust","Webb-Ventures/typescript-rust"]
REPO=Webb-Ventures/tsc-rust
TRUSTED_USERS=["riley-wv"]
MODEL=gpt-6.1-sol
EFFORT=high
# From create-github-app.mjs:
GITHUB_APP_ID=
CONFIG
  chmod 600 "$DATA/config.env"
fi

DOCKER_GID="$(getent group docker | cut -d: -f3)" docker compose -f "$src/box/compose.yaml" up -d --build --remove-orphans
docker image prune -f --filter label=com.docker.compose.project=ts-r-review >/dev/null

# Tailnet-only entry point: https://<box>.ts.net/code-review/ -> the dispatcher.
sudo tailscale serve --bg --https=443 --set-path=/code-review http://127.0.0.1:8787 >/dev/null
sudo tailscale serve status

if [ ! -f "$DATA/codex-home/auth.json" ]; then
  cat <<HINT
Codex is not logged in yet. Sign in a ChatGPT login for the box (device code):
  docker run --rm -it -v $DATA/codex-home:/codex -e CODEX_HOME=/codex -e HOME=/tmp ts-r-review:latest \\
    codex login --device-auth -c 'cli_auth_credentials_store="file"'
HINT
fi
