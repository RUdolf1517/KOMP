#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "${KOMP_NO_AUTO_UPDATE:-}" == "1" ]]; then
  echo "Auto-update disabled by KOMP_NO_AUTO_UPDATE=1"
  exit 0
fi

if ! command -v git >/dev/null 2>&1 || [[ ! -d "$ROOT_DIR/.git" ]]; then
  exit 0
fi

branch="$(git branch --show-current 2>/dev/null || true)"
if [[ -z "$branch" ]]; then
  echo "Auto-update skipped: detached HEAD"
  exit 0
fi

if ! git remote get-url origin >/dev/null 2>&1; then
  echo "Auto-update skipped: origin remote is not configured"
  exit 0
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Auto-update skipped: tracked files have local changes"
  exit 0
fi

echo "Checking for KOMP updates on origin/$branch..."
if ! git fetch --quiet origin "$branch"; then
  echo "Auto-update skipped: git fetch failed"
  exit 0
fi

local_rev="$(git rev-parse HEAD)"
remote_rev="$(git rev-parse "origin/$branch")"
base_rev="$(git merge-base HEAD "origin/$branch")"

if [[ "$local_rev" == "$remote_rev" ]]; then
  echo "KOMP is up to date."
elif [[ "$local_rev" == "$base_rev" ]]; then
  echo "Updating KOMP to $remote_rev..."
  git pull --ff-only origin "$branch"
  echo "KOMP updated. Cargo will rebuild if needed."
else
  echo "Auto-update skipped: local branch diverged from origin/$branch"
fi
