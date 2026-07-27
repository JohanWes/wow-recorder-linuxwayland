#!/usr/bin/env bash
set -euo pipefail

# Final AppImage migration helper. It performs a user-level Flatpak install,
# uninstalls the AppImage it replaces, and starts the native app. The permanent
# remote and the desktop software center own every update after this one.
#
# The final AppImage's updater pipes this script into bash and may append its
# own `--prefix <dir>` argument. Arguments are deliberately ignored: there is
# no prefix to install into any more.

REMOTE_NAME="warcraft-recorder"
REMOTE_DESCRIPTOR="${WARCRAFTRECORDER_REMOTE_DESCRIPTOR:-https://johanwes.github.io/wow-recorder-linuxwayland/index.flatpakrepo}"
FLATHUB_DESCRIPTOR="${WARCRAFTRECORDER_FLATHUB_DESCRIPTOR:-https://dl.flathub.org/repo/flathub.flatpakrepo}"
INSTALL_PAGE="${WARCRAFTRECORDER_INSTALL_PAGE:-https://github.com/JohanWes/wow-recorder-linuxwayland#install-and-update}"
APP_ID="io.github.JohanWes.WarcraftRecorder"
# Always address the published branch: a leftover development install of the
# same application ID would otherwise make a bare `flatpak run` ambiguous.
APP_REF="${APP_ID}//stable"
# Kept in step with `runtime-version` in the release manifest.
RUNTIME_REF="org.gnome.Platform//50"

LEGACY_BIN="${WARCRAFTRECORDER_LEGACY_BIN:-${HOME}/.local/bin/warcraftrecorder}"
LEGACY_DESKTOP="${WARCRAFTRECORDER_LEGACY_DESKTOP:-${HOME}/.local/share/applications/warcraftrecorder.desktop}"
LEGACY_ICON="${WARCRAFTRECORDER_LEGACY_ICON:-${HOME}/.local/share/icons/hicolor/256x256/apps/warcraftrecorder.png}"
AUTOSTART_DIR="${WARCRAFTRECORDER_AUTOSTART_DIR:-${HOME}/.config/autostart}"
# Where an earlier revision of this script parked the AppImage instead of
# deleting it. Still cleaned up, so a second migration finishes the job.
RETIRED_APPIMAGE="${WARCRAFTRECORDER_RETIRED_APPIMAGE:-${HOME}/.local/share/warcraftrecorder/WarcraftRecorder-final.AppImage}"

log() { printf '[install] %s\n' "$*"; }
warn() { printf '[install] WARNING: %s\n' "$*" >&2; }

manual_instructions() {
  printf '[install] Native Flatpak migration is not automatic on this system.\n' >&2
  printf '[install] Installation page: %s\n' "$INSTALL_PAGE" >&2
  printf '[install] Manual commands:\n' >&2
  printf '  flatpak remote-add --user --if-not-exists flathub %s\n' "$FLATHUB_DESCRIPTOR" >&2
  printf '  flatpak remote-add --user --if-not-exists %s %s\n' \
    "$REMOTE_NAME" "$REMOTE_DESCRIPTOR" >&2
  printf '  flatpak install --user %s %s\n' "$REMOTE_NAME" "$APP_REF" >&2
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

# One launcher, one app. The AppImage is deleted rather than parked: leaving a
# second, self-updating copy of Warcraft Recorder on disk is the exact thing
# this migration exists to end. Rolling back means downloading the 7.7.1
# release again, which still carries the AppImage and its checksum.
remove_appimage_install() {
  if [[ -f "$LEGACY_BIN" ]] && ! grep -q "flatpak run ${APP_ID}" "$LEGACY_BIN" 2>/dev/null; then
    if rm -f "$LEGACY_BIN"; then
      log "Removed the AppImage: ${LEGACY_BIN}"
      cat >"$LEGACY_BIN" <<EOF
#!/usr/bin/env bash
# Warcraft Recorder is a Flatpak now; this shim keeps the old command working.
exec flatpak run ${APP_REF} "\$@"
EOF
      chmod 755 "$LEGACY_BIN"
      log "Rewrote ${LEGACY_BIN} to launch the Flatpak"
    else
      warn "could not remove ${LEGACY_BIN}; the old launcher is still in place"
    fi
  fi

  if [[ -f "$RETIRED_APPIMAGE" ]]; then
    rm -f "$RETIRED_APPIMAGE" && log "Removed the parked AppImage: ${RETIRED_APPIMAGE}"
    # The AppImage installer's metadata directory: the version marker it holds
    # describes a build that is no longer on this machine.
    rm -f "$(dirname "$RETIRED_APPIMAGE")/release-tag"
    rmdir "$(dirname "$RETIRED_APPIMAGE")" 2>/dev/null || true
  fi

  # The Flatpak installs its own menu entry and icon; keeping the AppImage's
  # would show two identical launchers.
  if [[ -f "$LEGACY_DESKTOP" ]] && grep -q "$LEGACY_BIN" "$LEGACY_DESKTOP" 2>/dev/null; then
    rm -f "$LEGACY_DESKTOP" && log "Removed the AppImage menu entry: ${LEGACY_DESKTOP}"
    rm -f "$LEGACY_ICON"
  fi

  # "Run at start-up" wrote an autostart entry pointing at the AppImage. Point
  # it at the Flatpak instead of silently launching the retired app at login.
  local entry
  for entry in "$AUTOSTART_DIR"/*.desktop; do
    [[ -f "$entry" ]] || continue
    grep -Eqi "^Exec=.*(${LEGACY_BIN}|[^[:space:]]*warcraftrecorder[^[:space:]]*\.AppImage)" \
      "$entry" || continue
    sed -i "s|^Exec=.*|Exec=flatpak run ${APP_REF}|" "$entry"
    log "Repointed the start-up entry at the Flatpak: ${entry}"
  done
}

# The 7.7.1 updater does not merely fail to quit: on a successful update it
# relaunches itself, and that replacement starts after this script has already
# returned. So a single signal at any one moment loses the race. Sweep by
# executable instead, for a window wide enough to cover the relaunch. The
# AppImage is deleted by now, so nothing legitimate can match. Best effort:
# never fail the migration over it.
close_running_appimage() {
  local watchdog
  watchdog='
legacy=$1
retired=$2
deadline=$(( $(date +%s) + 30 ))
attempt=0
while [ "$(date +%s)" -lt "$deadline" ]; do
  attempt=$((attempt + 1))
  # Electron leaves on TERM; escalate only if ten sweeps went unanswered.
  if [ "$attempt" -gt 10 ]; then signal=KILL; else signal=TERM; fi
  for entry in /proc/[0-9]*; do
    exe=$(readlink "$entry/exe" 2>/dev/null) || continue
    # A running binary that has been deleted keeps its path plus this suffix.
    exe=${exe% (deleted)}
    case "$exe" in
      "$legacy" | "$retired" | /tmp/.mount_*/warcraftrecorder) ;;
      *) continue ;;
    esac
    kill "-${signal}" "${entry#/proc/}" 2>/dev/null || true
  done
  sleep 1
done'
  if command -v setsid >/dev/null 2>&1; then
    setsid sh -c "$watchdog" wr-migration "$LEGACY_BIN" "$RETIRED_APPIMAGE" \
      >/dev/null 2>&1 </dev/null &
  else
    sh -c "$watchdog" wr-migration "$LEGACY_BIN" "$RETIRED_APPIMAGE" \
      >/dev/null 2>&1 </dev/null &
  fi
  log "Closing the retired AppImage."
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
if ! flatpak install --user --assumeyes --or-update "$REMOTE_NAME" "$APP_REF"; then
  fail_with_manual_instructions
fi

log "Checksum verified (Flatpak remote signature verified by Flatpak)."
log "Installed binary: Flatpak $APP_ID"

remove_appimage_install

# Start the native app here. stdio goes to /dev/null: the updater waits for this
# script's pipes to close before it reports success.
log "Launching the native app..."
if command -v setsid >/dev/null 2>&1; then
  setsid flatpak run "$APP_REF" >/dev/null 2>&1 </dev/null &
else
  flatpak run "$APP_REF" >/dev/null 2>&1 </dev/null &
fi

close_running_appimage

log "Done. Warcraft Recorder is now a Flatpak and updates through it."
