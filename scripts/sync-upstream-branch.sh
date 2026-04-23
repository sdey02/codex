#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <branch> <upstream-url>" >&2
  exit 1
fi

branch="$1"
upstream_url="$2"

git remote get-url upstream >/dev/null 2>&1 || git remote add upstream "$upstream_url"
git remote set-url upstream "$upstream_url"

git fetch origin "$branch"
git fetch upstream "$branch"
git checkout "$branch"

before_rebase="$(git rev-parse HEAD)"
git rebase "upstream/$branch"
after_rebase="$(git rev-parse HEAD)"

if [[ "$before_rebase" == "$after_rebase" ]]; then
  git push origin "$branch"
else
  git push --force-with-lease origin "$branch"
fi
