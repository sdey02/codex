#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <branch> <upstream-url>" >&2
  exit 1
fi

branch="$1"
upstream_url="$2"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree must be clean before syncing" >&2
  exit 1
fi

git remote get-url upstream >/dev/null 2>&1 || git remote add upstream "$upstream_url"
git remote set-url upstream "$upstream_url"

git fetch origin "$branch"
git fetch upstream "$branch"

origin_before="$(git rev-parse "origin/$branch")"
backup_branch="backup/${branch//\//-}-before-upstream-sync-$(date -u +%Y%m%d%H%M%S)"

git branch "$backup_branch" "$origin_before"

if git show-ref --verify --quiet "refs/heads/$branch"; then
  git switch "$branch"
  git reset --hard "origin/$branch"
else
  git switch -c "$branch" "origin/$branch"
fi

before_rebase="$(git rev-parse HEAD)"
git rebase "upstream/$branch"
after_rebase="$(git rev-parse HEAD)"

if [[ -d .github/workflows ]]; then
  mapfile -t workflow_files < <(find .github/workflows -maxdepth 1 -type f ! -name README.md -print)
  if [[ ${#workflow_files[@]} -gt 0 ]]; then
    git rm -f "${workflow_files[@]}"
    git commit -m "chore: remove upstream GitHub Actions workflows on fork"
  fi
fi

if [[ "$before_rebase" == "$after_rebase" ]]; then
  git push origin "$branch"
else
  git push --force-with-lease="refs/heads/$branch:$origin_before" origin "$branch"
fi
