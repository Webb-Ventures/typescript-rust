#!/usr/bin/env bash
# Usage: preview-cleanup.sh [pr-number...]
# Deletes the Cloudflare preview Worker ts-r-preview-pr-<n> and its custom domain
# preview-pr-<n>.ts-r.dev for each pull request given; with no arguments, for every preview
# Worker whose pull request is closed or merged. Needs CLOUDFLARE_API_TOKEN,
# CLOUDFLARE_ACCOUNT_ID, GH_TOKEN and GITHUB_REPOSITORY. Used by preview-cleanup.yml.
set -euo pipefail
api="https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID"
cf() { curl -fsS -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" "$@"; }

if [ $# -eq 0 ]; then
  for n in $(cf "$api/workers/scripts" | jq -r '.result[].id | capture("^ts-r-preview-pr-(?<n>[0-9]+)$").n'); do
    state=$(gh pr view "$n" --repo "$GITHUB_REPOSITORY" --json state -q .state)
    if [ "$state" = CLOSED ] || [ "$state" = MERGED ]; then set -- "$@" "$n"; fi
  done
  [ $# -gt 0 ] || { echo 'no previews to remove'; exit 0; }
fi

for n in "$@"; do
  case $n in *[!0-9]* | '') echo "not a pull request number: $n" >&2; exit 1 ;; esac
  name="ts-r-preview-pr-$n" host="preview-pr-$n.ts-r.dev"
  for id in $(cf "$api/workers/domains?hostname=$host" | jq -r '.result[].id'); do
    cf -X DELETE "$api/workers/domains/$id" > /dev/null
    echo "removed $host"
  done
  if cf -o /dev/null "$api/workers/scripts/$name" 2> /dev/null; then
    cf -X DELETE "$api/workers/scripts/$name?force=true" > /dev/null
    echo "deleted Worker $name"
  else
    echo "no Worker $name"
  fi
done
