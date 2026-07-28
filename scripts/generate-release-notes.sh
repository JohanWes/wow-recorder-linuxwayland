#!/usr/bin/env bash
# Prepend the commit subjects since the previous release tag to
# data/release-notes.md, which the "What's new" dialog reads.
#
# Run this immediately before the release commit: that commit is the one
# carrying the generated section, so it cannot list itself.
set -euo pipefail

version="${1:-}"
if [[ -z "$version" ]]; then
  printf 'usage: %s X.Y.Z\n' "$0" >&2
  exit 2
fi

notes=data/release-notes.md
test -f "$notes"

if grep -q "^## $version\$" "$notes"; then
  printf 'release notes for %s already exist in %s\n' "$version" "$notes" >&2
  exit 1
fi

previous=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
range="${previous:+$previous..}HEAD"

subjects=$(git log --no-merges --reverse --format='- %s' "$range")
if [[ -z "$subjects" ]]; then
  printf 'no commits since %s\n' "${previous:-the start of history}" >&2
  exit 1
fi

header=$(sed -n '/^## /q;p' "$notes")
body=$(sed -n '/^## /,$p' "$notes")

{
  printf '%s\n\n' "$header"
  printf '## %s\n%s\n' "$version" "$subjects"
  if [[ -n "$body" ]]; then
    printf '\n%s\n' "$body"
  fi
} > "$notes.tmp"
mv "$notes.tmp" "$notes"

printf 'wrote %s notes for %s (%s)\n' \
  "$(printf '%s\n' "$subjects" | wc -l)" "$version" "$range"
