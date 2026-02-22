#!/usr/bin/env bash
set -euo pipefail

# Run from repo root regardless of caller location.
cd "$(git rev-parse --show-toplevel)"

pro_paths=(
  "src/storage/CloudClient.ts"
  "src/renderer/CloudSettings.tsx"
  "src/renderer/PatreonButton.tsx"
  "src/renderer/VideoChat.tsx"
  "src/renderer/ConfirmChatNamePrompt.tsx"
  "src/renderer/containers/ApplicationStatusCard/CloudStatus.tsx"
  "src/renderer/containers/ApplicationStatusCard/CloudStatusCard.tsx"
  "src/renderer/BulkTransferDialog.tsx"
  "src/renderer/StorageFilterToggle.tsx"
  "src/types/api.ts"
)

banned_patterns=(
  "from 'storage/CloudClient'"
  "CloudClient.getInstance()"
  "reconfigureCloud"
  "refreshCloudGuilds"
  "getOrCreateChatCorrelator"
  "getChatMessages("
  "postChatMessage("
  "deleteChatMessage("
  "TabsTrigger value=\"pro\""
  "TabsContent value=\"pro\""
)

failed=0

for f in "${pro_paths[@]}"; do
  if [ -e "$f" ]; then
    echo "[check-no-pro] Found forbidden path: $f"
    failed=1
  fi
done

for p in "${banned_patterns[@]}"; do
  if rg -n -S "$p" src .github/workflows >/dev/null 2>&1; then
    echo "[check-no-pro] Found forbidden pattern: $p"
    rg -n -S "$p" src .github/workflows || true
    failed=1
  fi
done

if [ "$failed" -ne 0 ]; then
  echo "[check-no-pro] FAILED"
  exit 1
fi

echo "[check-no-pro] OK"
