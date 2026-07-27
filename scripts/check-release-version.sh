#!/usr/bin/env bash
set -euo pipefail

tag="${1:-}"
if [[ -z "$tag" || "$tag" != v* ]]; then
  printf 'usage: %s vX.Y.Z\n' "$0" >&2
  exit 2
fi

version="${tag#v}"
cargo_version=$(awk -F'"' '/^version = "/ { print $2; exit }' native/Cargo.toml)
appstream_version=$(awk -F'"' '/<release version="/ { print $2; exit }' data/io.github.JohanWes.WarcraftRecorder.metainfo.xml)

if [[ "$version" != "$cargo_version" || "$version" != "$appstream_version" ]]; then
  printf 'release version mismatch: tag=%s Cargo=%s AppStream=%s\n' \
    "$version" "$cargo_version" "$appstream_version" >&2
  exit 1
fi

printf 'release version: %s (Cargo.toml and AppStream agree)\n' "$version"
