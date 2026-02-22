#!/usr/bin/env bash
set -euo pipefail

# Run from repo root regardless of caller location.
cd "$(git rev-parse --show-toplevel)"

windows_paths=(
  "src/main/Recorder.ts"
  "src/main/AppUpdater.ts"
  "binaries/rust-ps.exe"
  "installer.nsh"
  "src/renderer/WindowsSettings.tsx"
  "src/renderer/ManualSettings.tsx"
  "src/renderer/SceneEditor.tsx"
  "src/renderer/RecorderPreview.tsx"
  "src/renderer/VideoSourceControls.tsx"
  "src/renderer/AudioSourceControls.tsx"
  "src/renderer/VideoBaseControls.tsx"
  "src/renderer/ChatOverlayControls.tsx"
  "src/renderer/containers/UpdateNotifier/UpdateNotifier.tsx"
)

banned_patterns=(
  "require('../Recorder')"
  "from './WindowsSettings'"
  "from './ManualSettings'"
  "from './SceneEditor'"
  "from './RecorderPreview'"
  "from './VideoSourceControls'"
  "from './AudioSourceControls'"
  "from './VideoBaseControls'"
  "from './ChatOverlayControls'"
  "from './AppUpdater'"
  "from './containers/UpdateNotifier/UpdateNotifier'"
)

failed=0

for f in "${windows_paths[@]}"; do
  if [ -e "$f" ]; then
    echo "[check-no-windows] Found forbidden path: $f"
    failed=1
  fi
done

for p in "${banned_patterns[@]}"; do
  if rg -n -S "$p" src .github/workflows >/dev/null 2>&1; then
    echo "[check-no-windows] Found forbidden pattern: $p"
    rg -n -S "$p" src .github/workflows || true
    failed=1
  fi
done

if [ "$failed" -ne 0 ]; then
  echo "[check-no-windows] FAILED"
  exit 1
fi

echo "[check-no-windows] OK"
