#!/usr/bin/env bash
# sync-back.sh: carries a pull request merged into this repo's main to the development repo
# ($PRIVATE_REPO) as branch public/pr-<n> and a pull request there. Run by sync-back.yml from a
# full clone of this repo.
#
# Env: PRIVATE_REPO, PRIVATE_TOKEN (contents, pull requests and workflows: write there),
# GH_TOKEN (read this repo), GITHUB_REPOSITORY, and either PR (a merged PR's number) or
# BEFORE/AFTER (the push to main).
set -euo pipefail

: "${PRIVATE_REPO:?}" "${GITHUB_REPOSITORY:?}"
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
note() { echo "$*"; echo "$*" >> "$summary"; }

if [ -z "${PRIVATE_TOKEN:-}" ]; then
  note "::warning::Secret GH_PRIVATE_REPO_TOKEN is not set; nothing is carried back."
  exit 0
fi

# The merged PR and the diff it made to main.
if [ -n "${PR:-}" ]; then
  gh api "repos/$GITHUB_REPOSITORY/pulls/$PR" > pr.json
  AFTER=$(jq -r '.merge_commit_sha // empty' pr.json)
  [ "$(jq -r '.merged' pr.json)" = true ] || { note "#$PR is not merged."; exit 1; }
  BEFORE=$(git rev-parse "$AFTER^1")
else
  : "${BEFORE:?}" "${AFTER:?}"
  gh api "repos/$GITHUB_REPOSITORY/commits/$AFTER/pulls" \
    --jq '[.[] | select(.merged_at != null and .base.ref == "main")] | .[0] // empty' > pr.json
  if [ ! -s pr.json ]; then
    note "$AFTER was pushed without a pull request; nothing to carry back."
    exit 0
  fi
  if ! git cat-file -e "$BEFORE^{commit}" 2>/dev/null; then
    note "::warning::The push has no earlier main ($BEFORE); nothing to carry back."
    exit 0
  fi
fi
number=$(jq -r .number pr.json)
head_ref=$(jq -r .head.ref pr.json)
head_repo=$(jq -r '.head.repo.full_name // ""' pr.json)
if [[ "$head_ref" == sync/tsc-rust-* && "$head_repo" == "$GITHUB_REPOSITORY" ]]; then
  note "#$number is a sync from the development repo; nothing to carry back."
  exit 0
fi
title=$(jq -r .title pr.json)
login=$(jq -r .user.login pr.json)
url=$(jq -r .html_url pr.json)

# The contributor as author: a squash merge's author is the PR's author; for a merge commit,
# the author of the PR's last commit.
src=$AFTER
if git rev-parse -q --verify "$AFTER^2" >/dev/null; then src=$(git rev-parse "$AFTER^2"); fi
author="$(git log -1 --format='%an <%ae>' "$src")"

work=$(mktemp -d)
auth=$(printf 'x-access-token:%s' "$PRIVATE_TOKEN" | base64 | tr -d '\n')
echo "::add-mask::$auth"
priv="$work/private"
# The header is stored in the clone's config: blobs are fetched lazily during the three-way
# apply. PRIVATE_URL overrides the remote, to rehearse against a local repo.
git clone -q --filter=blob:none -c "http.https://github.com/.extraheader=AUTHORIZATION: basic $auth" \
  "${PRIVATE_URL:-https://github.com/$PRIVATE_REPO}" "$priv"

# Files that are public-only live under .public/ there.
changed=()
while IFS= read -r f; do changed+=("$f"); done < <(git diff --name-only --no-renames "$BEFORE" "$AFTER")
overlay=()
plain=()
for f in ${changed[@]+"${changed[@]}"}; do
  if git -C "$priv" cat-file -e "origin/main:.public/$f" 2>/dev/null; then overlay+=("$f"); else plain+=("$f"); fi
done

branch="public/pr-$number"
git -C "$priv" checkout -q -b "$branch" origin/main
conflicts=false
apply() { # <patch> [--directory=<dir>]
  [ -s "$1" ] || return 0
  if ! git -C "$priv" apply --3way --whitespace=nowarn "${@:2}" "$1"; then
    if [ -n "$(git -C "$priv" diff --name-only --diff-filter=U)" ]; then
      conflicts=true
    else
      # Nothing applied: keep the change reviewable as a patch file.
      cp "$1" "$priv/public-pr-$number-$(basename "$1")"
      conflicts=true
    fi
  fi
}
if [ ${#plain[@]} -gt 0 ]; then
  git diff --no-renames --binary --full-index "$BEFORE" "$AFTER" -- "${plain[@]}" > "$work/plain.patch"
  apply "$work/plain.patch"
fi
if [ ${#overlay[@]} -gt 0 ]; then
  git diff --no-renames --binary --full-index "$BEFORE" "$AFTER" -- "${overlay[@]}" > "$work/overlay.patch"
  apply "$work/overlay.patch" --directory=.public
fi
git -C "$priv" add -A
if git -C "$priv" diff --cached --quiet; then
  note "#$number changes nothing in $PRIVATE_REPO (already there)."
  exit 0
fi

{
  echo "$title (public #$number)"
  echo
  echo "Carried over from $GITHUB_REPOSITORY#$number by @$login."
  echo
  echo "Public-Commit: $AFTER"
} > "$work/msg"
git -C "$priv" -c user.name='github-actions[bot]' \
  -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
  commit -q --author="$author" -F "$work/msg"
git -C "$priv" push -q -f origin "$branch"

{
  echo "Carries over $url, merged in the public repo, by @$login."
  echo
  echo "**Files**"
  echo
  for f in "${changed[@]}"; do echo "- \`$f\`"; done
  echo
  if [ "$conflicts" = true ]; then
    echo "> [!WARNING]"
    echo "> The diff did not apply cleanly to main here: resolve the conflict markers (or the \`.patch\` file) before merging."
    echo
  fi
  if printf '%s\n' "${changed[@]}" | grep -q '^\.github/'; then
    echo "> [!IMPORTANT]"
    echo "> This changes \`.github/\`: check workflows and their permissions before merging."
    echo
  fi
  echo "Opened by the public repo's sync-back workflow."
} > "$work/body.md"
draft=()
if [ "$conflicts" = true ]; then draft=(--draft); fi
existing=$(GH_TOKEN=$PRIVATE_TOKEN gh pr list -R "$PRIVATE_REPO" --head "$branch" --state open --json url --jq '.[0].url // empty')
if [ -n "$existing" ]; then
  note "Updated $existing."
else
  pr_url=$(GH_TOKEN=$PRIVATE_TOKEN gh pr create -R "$PRIVATE_REPO" --base main --head "$branch" \
    --title "Public #$number: $title" --body-file "$work/body.md" ${draft[@]+"${draft[@]}"})
  note "Opened $pr_url."
fi
