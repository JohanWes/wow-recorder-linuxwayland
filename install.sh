#!/usr/bin/env bash
set -euo pipefail

# Final AppImage migration helper. It performs a user-level Flatpak install,
# retires the AppImage launchers it originally created, and starts the native
# app. The permanent remote and the desktop software center own every update
# after this one.
#
# The final AppImage's updater pipes this script into bash and may append its
# own `--prefix <dir>` argument. Arguments are deliberately ignored: there is
# no prefix to install into any more.

REMOTE_NAME="warcraft-recorder"
REMOTE_DESCRIPTOR="${WARCRAFTRECORDER_REMOTE_DESCRIPTOR:-https://johanwes.github.io/wow-recorder-linuxwayland/index.flatpakrepo}"
FLATHUB_DESCRIPTOR="${WARCRAFTRECORDER_FLATHUB_DESCRIPTOR:-https://dl.flathub.org/repo/flathub.flatpakrepo}"
INSTALL_PAGE="${WARCRAFTRECORDER_INSTALL_PAGE:-https://github.com/JohanWes/wow-recorder-linuxwayland#install-and-update}"
APP_ID="io.github.JohanWes.WarcraftRecorder"
# Kept in step with `runtime-version` in the release manifest.
RUNTIME_REF="org.gnome.Platform//50"

LEGACY_BIN="${WARCRAFTRECORDER_LEGACY_BIN:-${HOME}/.local/bin/warcraftrecorder}"
LEGACY_DESKTOP="${WARCRAFTRECORDER_LEGACY_DESKTOP:-${HOME}/.local/share/applications/warcraftrecorder.desktop}"
AUTOSTART_DIR="${WARCRAFTRECORDER_AUTOSTART_DIR:-${HOME}/.config/autostart}"
PRESERVED_APPIMAGE="${WARCRAFTRECORDER_PRESERVED_APPIMAGE:-${HOME}/.local/share/warcraftrecorder/WarcraftRecorder-final.AppImage}"

log() { printf '[install] %s\n' "$*"; }
warn() { printf '[install] WARNING: %s\n' "$*" >&2; }

manual_instructions() {
  printf '[install] Native Flatpak migration is not automatic on this system.\n' >&2
  printf '[install] Installation page: %s\n' "$INSTALL_PAGE" >&2
  printf '[install] Manual commands:\n' >&2
  printf '  flatpak remote-add --user --if-not-exists flathub %s\n' "$FLATHUB_DESCRIPTOR" >&2
  printf '  flatpak remote-add --user --if-not-exists %s %s\n' \
    "$REMOTE_NAME" "$REMOTE_DESCRIPTOR" >&2
  printf '  flatpak install --user %s %s\n' "$REMOTE_NAME" "$APP_ID" >&2
}

fail_with_manual_instructions() {
  manual_instructions
  return 1
}

# The project remote carries the application only. Without a remote that
# publishes the GNOME runtime the install fails on an unresolvable dependency,
# so make sure one is reachable before asking for the app.
ensure_runtime_source() {
  if flatpak info "$RUNTIME_REF" >/dev/null 2>&1; then
    return 0
  fi
  if flatpak remotes --columns=name | grep -qx flathub; then
    return 0
  fi
  log "Adding Flathub for the GNOME runtime..."
  flatpak remote-add --user --if-not-exists flathub "$FLATHUB_DESCRIPTOR" ||
    warn "could not add Flathub; the runtime must come from an existing remote"
}

# One launcher, one app: the shim and the menu entry the AppImage installer
# created now belong to the Flatpak. The AppImage itself is kept, so a rollback
# is still one command.
retire_appimage_launchers() {
  if [[ -f "$LEGACY_BIN" ]] && ! grep -q "flatpak run ${APP_ID}" "$LEGACY_BIN" 2>/dev/null; then
    if mkdir -p "$(dirname "$PRESERVED_APPIMAGE")" &&
      mv -f "$LEGACY_BIN" "$PRESERVED_APPIMAGE"; then
      log "Preserved the final AppImage: ${PRESERVED_APPIMAGE}"
      cat >"$LEGACY_BIN" <<EOF
#!/usr/bin/env bash
# Warcraft Recorder is a Flatpak now. The final AppImage was preserved at
# ${PRESERVED_APPIMAGE} and can be run directly to roll back.
exec flatpak run ${APP_ID} "\$@"
EOF
      chmod 755 "$LEGACY_BIN"
      log "Rewrote ${LEGACY_BIN} to launch the Flatpak"
    else
      warn "could not move ${LEGACY_BIN}; the old launcher is still in place"
    fi
  fi

  # The Flatpak installs its own menu entry; keeping the AppImage one would
  # show two identical launchers.
  if [[ -f "$LEGACY_DESKTOP" ]] && grep -q "$LEGACY_BIN" "$LEGACY_DESKTOP" 2>/dev/null; then
    rm -f "$LEGACY_DESKTOP" && log "Removed the AppImage menu entry: ${LEGACY_DESKTOP}"
  fi

  # "Run at start-up" wrote an autostart entry pointing at the AppImage. Point
  # it at the Flatpak instead of silently launching the retired app at login.
  local entry
  for entry in "$AUTOSTART_DIR"/*.desktop; do
    [[ -f "$entry" ]] || continue
    grep -Eqi "^Exec=.*(${LEGACY_BIN}|[^[:space:]]*warcraftrecorder[^[:space:]]*\.AppImage)" \
      "$entry" || continue
    sed -i "s|^Exec=.*|Exec=flatpak run ${APP_ID}|" "$entry"
    log "Repointed the start-up entry at the Flatpak: ${entry}"
  done
}

if ! command -v flatpak >/dev/null 2>&1; then
  fail_with_manual_instructions
fi

# Keep these marker shapes compatible with the final AppImage updater's
# legacy parser. They describe migration phases, not a new AppImage download.
log "Downloading WarcraftRecorder.AppImage migration instructions..."

ensure_runtime_source

if ! flatpak remote-add --user --if-not-exists "$REMOTE_NAME" "$REMOTE_DESCRIPTOR"; then
  fail_with_manual_instructions
fi

# `--or-update` keeps a repeated migration (or a plain update) from failing on
# an already-installed application.
if ! flatpak install --user --assumeyes --or-update "$REMOTE_NAME" "$APP_ID"; then
  fail_with_manual_instructions
fi

log "Checksum verified (Flatpak remote signature verified by Flatpak)."
log "Installed binary: Flatpak $APP_ID"

retire_appimage_launchers

# The AppImage updater quits itself after this script returns, so start the
# native app here. stdio goes to /dev/null: the updater waits for this script's
# pipes to close before it reports success.
log "Launching the native app..."
if command -v setsid >/dev/null 2>&1; then
  setsid flatpak run "$APP_ID" >/dev/null 2>&1 </dev/null &
else
  flatpak run "$APP_ID" >/dev/null 2>&1 </dev/null &
fi

log "Done. Warcraft Recorder now updates through Flatpak; roll back by running ${PRESERVED_APPIMAGE}."
