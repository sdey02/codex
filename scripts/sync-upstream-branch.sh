#!/usr/bin/env bash
set -euo pipefail

TARGET_BRANCH="${1:-main}"
UPSTREAM_REPO="${2:-https://github.com/openai/codex.git}"

echo "Syncing origin/${TARGET_BRANCH} with ${UPSTREAM_REPO}:${TARGET_BRANCH}"

if ! git remote get-url upstream >/dev/null 2>&1; then
  git remote add upstream "${UPSTREAM_REPO}"
else
  git remote set-url upstream "${UPSTREAM_REPO}"
fi

git fetch origin "${TARGET_BRANCH}"
git fetch upstream "${TARGET_BRANCH}"

before_rev="$(git rev-parse "origin/${TARGET_BRANCH}")"
git checkout -B "${TARGET_BRANCH}" "origin/${TARGET_BRANCH}"

git config user.name "${GIT_AUTHOR_NAME:-github-actions[bot]}"
git config user.email "${GIT_AUTHOR_EMAIL:-41898282+github-actions[bot]@users.noreply.github.com}"

git rebase "upstream/${TARGET_BRANCH}"

after_rev="$(git rev-parse HEAD)"

if [[ "${before_rev}" == "${after_rev}" ]]; then
  echo "No upstream changes to apply."
  exit 0
fi

if git merge-base --is-ancestor "${before_rev}" "${after_rev}"; then
  echo "Fast-forward update detected; pushing normally."
  git push origin "${TARGET_BRANCH}"
else
  echo "Branch history was rewritten during rebase; pushing with --force-with-lease."
  git push --force-with-lease origin "${TARGET_BRANCH}"
fi

echo "Sync complete: ${before_rev} -> ${after_rev}"
