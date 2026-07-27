#!/usr/bin/env bash
set -euo pipefail

# Final AppImage migration helper. It intentionally performs only a user-level
# Flatpak install; the permanent remote and the desktop software center own
# future updates.

REMOTE_NAME="warcraft-recorder"
REMOTE_DESCRIPTOR="${WARCRAFTRECORDER_REMOTE_DESCRIPTOR:-https://johanwes.github.io/wow-recorder-linuxwayland/index.flatpakrepo}"
INSTALL_PAGE="${WARCRAFTRECORDER_INSTALL_PAGE:-https://github.com/JohanWes/wow-recorder-linuxwayland#install-and-update}"
APP_ID="io.github.JohanWes.WarcraftRecorder"

log() { printf '[install] %s\n' "$*"; }

manual_instructions() {
  printf '[install] Native Flatpak migration is not automatic on this system.\n' >&2
  printf '[install] Installation page: %s\n' "$INSTALL_PAGE" >&2
  printf '[install] Manual commands:\n' >&2
  printf '  flatpak remote-add --user --if-not-exists %s %s\n' \
    "$REMOTE_NAME" "$REMOTE_DESCRIPTOR" >&2
  printf '  flatpak install --user %s %s\n' "$REMOTE_NAME" "$APP_ID" >&2
}

fail_with_manual_instructions() {
  manual_instructions
  return 1
}

if ! command -v flatpak >/dev/null 2>&1; then
  fail_with_manual_instructions
fi

# Keep these marker shapes compatible with the final AppImage updater's
# legacy parser. They describe migration phases, not a new AppImage download.
log "Downloading WarcraftRecorder.AppImage migration instructions..."

if ! flatpak remote-add --user --if-not-exists "$REMOTE_NAME" "$REMOTE_DESCRIPTOR"; then
  fail_with_manual_instructions
fi

if ! flatpak install --user --assumeyes "$REMOTE_NAME" "$APP_ID"; then
  fail_with_manual_instructions
fi

log "Checksum verified (Flatpak remote signature verified by Flatpak)."
log "Installed binary: Flatpak $APP_ID"

if [[ -t 0 ]]; then
  read -r -p "Launch Warcraft Recorder now? [y/N] " answer
  case "$answer" in
    y|Y|yes|YES)
      flatpak run "$APP_ID" &
      ;;
  esac
else
  log "Launch with 'flatpak run $APP_ID'."
fi

log "Done. Native releases are Flatpak-only; the final AppImage remains available for rollback."
