#!/usr/bin/env bash
# Count all changed text lines, including manifests, tests and generated locks.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
check() {
  local label=$1
  awk -v label="$label" '
    $1 == "-" || $2 == "-" { binary = 1 }
    { added += $1; deleted += $2 }
    END {
      total = added + deleted
      printf "%s +%d -%d total=%d\n", label, added, deleted, total
      if (binary || total > 1000) {
        print "Rejected: binary diff or more than 1000 changed lines" > "/dev/stderr"
        exit 1
      }
    }'
}
case "${1:---staged}" in
  --staged)
    [[ $# -le 1 ]] || exit 2
    git diff --cached --numstat --no-renames | check staged
    ;;
  *)
    [[ $# == 1 ]] || { printf 'Usage: bash scripts/check-commit-size.sh [--staged|--all|BASE..HEAD]\n' >&2; exit 2; }
    revision=$1
    # Inspect the current branch's complete history without unrelated refs.
    [[ "$revision" != --all ]] || revision=HEAD
    commits=$(git rev-list --reverse "$revision")
    while read -r commit; do
      [[ -n "$commit" ]] || continue
      git diff-tree --root --no-commit-id --numstat --no-renames -r "$commit" | check "$commit"
    done <<< "$commits"
    ;;
esac
