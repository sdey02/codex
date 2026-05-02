---
name: sync-openai-upstream
description: Use when syncing the sdey02/codex fork with OpenAI's upstream codex main branch, rebasing fork-only changes, preserving fork-specific account UI work, and keeping GitHub Actions workflows disabled.
metadata:
  short-description: Sync sdey02/codex with upstream OpenAI Codex
---

# Sync OpenAI Upstream

Use this skill when asked to bring the `sdey02/codex` fork up to date with `openai/codex`.

## Required Intent

Preserve these fork-owned changes while taking upstream updates:

- Multi-account login/status/removal and TUI account popup/rate-window behavior.
- No active GitHub Actions workflows in `.github/workflows/`; keep only inert documentation such as `README.md`.
- The upstream sync helper script and this skill.

## Workflow

1. Start read-only:
   - Check `git status --short --branch`.
   - Verify remotes: `origin` should be `git@github.com:sdey02/codex.git`; `upstream` should be `https://github.com/openai/codex.git`.
   - Fetch `origin main` and `upstream main`.
   - Inspect fork delta with `git log --oneline upstream/main..origin/main` and `git diff --stat upstream/main..origin/main`.
2. Sync by rebase:
   - Create a work branch from `origin/main`, for example `sync/openai-upstream-main`.
   - Rebase onto `upstream/main`.
   - Resolve conflicts by keeping upstream structure unless it conflicts with the fork-owned changes listed above.
   - Remove every active file under `.github/workflows/` except `README.md`.
3. After conflict resolution:
   - Add or update this skill if future sync instructions changed.
   - Keep changes minimal and reviewable.

## Verification

Run the relevant checks before pushing:

- `git status --short --branch`
- `git log --oneline upstream/main..HEAD`
- `git diff --stat upstream/main..HEAD`
- From `codex-rs`: `just fmt`
- From `codex-rs`: `cargo test -p codex-login`
- From `codex-rs`: `cargo test -p codex-tui`
- If TUI snapshots intentionally changed, inspect pending snapshots and accept them with `cargo insta accept -p codex-tui`.
- Run scoped fixes for touched Rust crates, such as `just fix -p codex-login` and `just fix -p codex-tui`.

Push the rebased branch to `origin/main` with `--force-with-lease` only after verification is complete.
