#!/usr/bin/env bash
set -euo pipefail

export WARCRAFTRECORDER_DISABLE_UPDATER="${WARCRAFTRECORDER_DISABLE_UPDATER:-1}"

appimage="/opt/warcraft-recorder-linux/WarcraftRecorder.AppImage"

extra_args=()
if [[ "${WARCRAFTRECORDER_NO_SANDBOX:-}" == "1" ]]; then
  extra_args+=(--no-sandbox)
fi

exec "${appimage}" "${extra_args[@]}" "$@"

