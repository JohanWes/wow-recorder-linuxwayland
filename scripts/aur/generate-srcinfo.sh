#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <aur_pkg_dir>" >&2
  exit 2
fi

aur_pkg_dir="$1"

docker run --rm \
  -v "$(realpath "${aur_pkg_dir}"):/pkg" \
  -w /pkg \
  archlinux:base-devel \
  bash -lc "set -euo pipefail; makepkg --printsrcinfo > .SRCINFO"

echo "Wrote: ${aur_pkg_dir}/.SRCINFO"

