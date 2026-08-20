#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Regenerate the bundled spell database (data/spells/) from wago.tools.
# Pass a build to pin it, e.g. 12.1.0.69382; omit for the latest retail.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v python3 >/dev/null 2>&1; then
  printf 'fetch-spell-data needs python3 with Pillow\n' >&2
  exit 2
fi

python3 scripts/fetch-spell-data.py "$@"
