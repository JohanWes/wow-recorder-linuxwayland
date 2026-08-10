#!/usr/bin/env bash
set -euo pipefail

# Installer for the Warcraft Recorder Flatpak. Safe to pipe into bash: it adds
# the project remote, installs the app, and starts it. Re-running it updates an
# existing install.
#
# It doubles as the final AppImage's migration helper. When invoked by the old
# updater it also removes that AppImage and closes the running copy. The updater
# may pass the AppImage's `--prefix <dir>` so its non-default launcher and menu
# files can be removed too; all other old installer arguments are ignored.

LEGACY_PREFIX="${HOME}/.local"
while [[ "$#" -gt 0 ]]; do
  if [[ "$1" == --prefix && -n "${2:-}" ]]; then
    LEGACY_PREFIX="$2"
    shift 2
  else
    shift
  fi
done

REMOTE_NAME="warcraft-recorder"
REMOTE_DESCRIPTOR="${WARCRAFTRECORDER_REMOTE_DESCRIPTOR:-https://johanwes.github.io/wow-recorder-linuxwayland/index.flatpakrepo}"
FLATHUB_DESCRIPTOR="${WARCRAFTRECORDER_FLATHUB_DESCRIPTOR:-https://dl.flathub.org/repo/flathub.flatpakrepo}"
INSTALL_PAGE="${WARCRAFTRECORDER_INSTALL_PAGE:-https://github.com/JohanWes/wow-recorder-linuxwayland#install}"
APP_ID="io.github.JohanWes.WarcraftRecorder"
# Always address the published branch: a leftover development install of the
# same application ID would otherwise make a bare `flatpak run` ambiguous.
APP_REF="${APP_ID}//stable"
# Kept in step with `runtime-version` in the release manifest.
RUNTIME_REF="org.gnome.Platform//50"

LEGACY_BIN="${WARCRAFTRECORDER_LEGACY_BIN:-${LEGACY_PREFIX}/bin/warcraftrecorder}"
LEGACY_DESKTOP="${WARCRAFTRECORDER_LEGACY_DESKTOP:-${LEGACY_PREFIX}/share/applications/warcraftrecorder.desktop}"
LEGACY_ICON="${WARCRAFTRECORDER_LEGACY_ICON:-${LEGACY_PREFIX}/share/icons/hicolor/256x256/apps/warcraftrecorder.png}"
LEGACY_METADATA="${WARCRAFTRECORDER_LEGACY_METADATA:-${LEGACY_PREFIX}/share/warcraftrecorder}"
AUTOSTART_DIR="${WARCRAFTRECORDER_AUTOSTART_DIR:-${HOME}/.config/autostart}"
# AppImage runtime paths survive through the old updater. Validate the mounted
# Warcraft Recorder executable before trusting APPIMAGE as a deletion target.
RUNNING_APPIMAGE=""
if [[ -n "${APPIMAGE:-}" && -x "${APPIMAGE:-}" &&
  -n "${APPDIR:-}" && -x "${APPDIR}/warcraftrecorder" ]]; then
  RUNNING_APPIMAGE="$APPIMAGE"
fi
# Where an earlier revision of this script parked the AppImage instead of
# deleting it. Still cleaned up, so a second migration finishes the job.
RETIRED_APPIMAGE="${WARCRAFTRECORDER_RETIRED_APPIMAGE:-${HOME}/.local/share/warcraftrecorder/WarcraftRecorder-final.AppImage}"

log() { printf '[install] %s\n' "$*"; }
warn() { printf '[install] WARNING: %s\n' "$*" >&2; }

manual_instructions() {
  printf '[install] Automatic Flatpak installation failed.\n' >&2
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

# Only an actual AppImage install triggers the migration steps. The shim a
# previous migration left behind is not one.
migrating_from_appimage() {
  [[ -n "$RUNNING_APPIMAGE" && -x "$RUNNING_APPIMAGE" ]] && return 0
  [[ -f "$RETIRED_APPIMAGE" ]] && return 0
  [[ -f "$LEGACY_BIN" ]] && ! grep -q "flatpak run ${APP_ID}" "$LEGACY_BIN" 2>/dev/null
}

# Flatpak itself is the one thing this script cannot install: it needs the
# distribution's package manager and a password.
require_flatpak() {
  command -v flatpak >/dev/null 2>&1 && return 0
  printf '[install] Flatpak is required and is not installed.\n' >&2
  printf '[install] Install it, log out and back in, then run this again:\n' >&2
  printf '  Arch, CachyOS, Manjaro:   sudo pacman -S flatpak\n' >&2
  printf '  Fedora, Nobara:           sudo dnf install flatpak\n' >&2
  printf '  Ubuntu, Debian, Mint:     sudo apt install flatpak\n' >&2
  printf '  openSUSE:                 sudo zypper install flatpak\n' >&2
  printf '[install] Installation page: %s\n' "$INSTALL_PAGE" >&2
  return 1
}

# The project remote carries the application only. Without a remote that
# publishes the GNOME runtime the install fails on an unresolvable dependency,
# so make sure one is reachable before asking for the app.
ensure_runtime_source() {
  if flatpak info "$RUNTIME_REF" >/dev/null 2>&1; then
    return 0
  fi
  if flatpak remotes --user --columns=name | grep -qx flathub; then
    return 0
  fi
  log "Adding Flathub for the GNOME runtime..."
  flatpak remote-add --user --if-not-exists flathub "$FLATHUB_DESCRIPTOR" ||
    warn "could not add Flathub; the runtime must come from an existing remote"
}

# Capture goes through the desktop's xdg-desktop-portal ScreenCast backend,
# and the frames arrive over PipeWire. Both are host components the Flatpak
# cannot carry, and a missing one shows up as a failed recording long after
# the install looked fine. A backend advertises the interfaces it implements
# in its `.portal` file; xdg-desktop-portal-gtk alone is not enough, because
# it does not implement ScreenCast.
screencast_backend_present() {
  local dir
  local -a data_dirs
  # Quoted iteration: a data dir is allowed to contain glob characters.
  IFS=: read -ra data_dirs <<<"${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
  for dir in "${data_dirs[@]}"; do
    if grep -qrs 'org\.freedesktop\.impl\.portal\.ScreenCast' \
      "${dir}/xdg-desktop-portal/portals"; then
      return 0
    fi
  done
  return 1
}

# Warnings only. None of this stops the app from installing or updating, and
# a user who fixes the session afterwards needs no second install.
check_session() {
  # Only x11 is a diagnosable problem. An unset or `tty` session type means
  # this is not running from the desktop the app will launch into, so the
  # checks below have nothing trustworthy to look at either.
  if [[ "${XDG_SESSION_TYPE:-}" == x11 ]]; then
    warn "this looks like an X11 session; Warcraft Recorder needs a Wayland session"
    return 0
  fi
  [[ -n "${WAYLAND_DISPLAY:-}" ]] || return 0
  if ! screencast_backend_present; then
    warn "no screen-capture portal found; install the portal backend for your desktop"
    warn "  before recording (for example xdg-desktop-portal-gnome, -kde,"
    warn "  -hyprland or -wlr)"
  fi
  if [[ ! -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pipewire-0" ]]; then
    warn "PipeWire does not look like it is running; the portal needs it to send frames"
  fi
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

  if [[ -n "$RUNNING_APPIMAGE" &&
    "$RUNNING_APPIMAGE" != "$LEGACY_BIN" &&
    "$RUNNING_APPIMAGE" != "$RETIRED_APPIMAGE" &&
    -x "$RUNNING_APPIMAGE" ]]; then
    if rm -f "$RUNNING_APPIMAGE"; then
      log "Removed the running AppImage: ${RUNNING_APPIMAGE}"
    else
      warn "could not remove ${RUNNING_APPIMAGE}; remove the old AppImage manually"
    fi
  fi

  if [[ -f "$RETIRED_APPIMAGE" ]]; then
    rm -f "$RETIRED_APPIMAGE" && log "Removed the parked AppImage: ${RETIRED_APPIMAGE}"
    rm -f "$(dirname "$RETIRED_APPIMAGE")/release-tag"
    rmdir "$(dirname "$RETIRED_APPIMAGE")" 2>/dev/null || true
  fi

  rm -f "$LEGACY_METADATA/release-tag"
  rmdir "$LEGACY_METADATA" 2>/dev/null || true

  # The Flatpak installs its own menu entry and icon; keeping the AppImage's
  # would show two identical launchers.
  if [[ -f "$LEGACY_DESKTOP" ]] &&
    { grep -Fq "$LEGACY_BIN" "$LEGACY_DESKTOP" 2>/dev/null ||
      { [[ -n "$RUNNING_APPIMAGE" ]] &&
        grep -Fq "$RUNNING_APPIMAGE" "$LEGACY_DESKTOP" 2>/dev/null; }; }; then
    rm -f "$LEGACY_DESKTOP" && log "Removed the AppImage menu entry: ${LEGACY_DESKTOP}"
    rm -f "$LEGACY_ICON"
  fi

  # "Run at start-up" wrote an autostart entry pointing at the AppImage. Point
  # it at the Flatpak instead of silently launching the retired app at login.
  local entry
  for entry in "$AUTOSTART_DIR"/*.desktop; do
    [[ -f "$entry" ]] || continue
    if ! grep -Fqi "$LEGACY_BIN" "$entry" 2>/dev/null &&
      ! { [[ -n "$RUNNING_APPIMAGE" ]] &&
        grep -Fqi "$RUNNING_APPIMAGE" "$entry" 2>/dev/null; } &&
      ! grep -Eqi "^Exec=.*[^[:space:]]*warcraftrecorder[^[:space:]]*\.AppImage" "$entry"; then
      continue
    fi
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
running=$3
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
      "$legacy" | "$retired" | "$running" | /tmp/.mount_*/warcraftrecorder) ;;
      *) continue ;;
    esac
    kill "-${signal}" "${entry#/proc/}" 2>/dev/null || true
  done
  sleep 1
done'
  if command -v setsid >/dev/null 2>&1; then
    setsid sh -c "$watchdog" wr-migration \
      "$LEGACY_BIN" "$RETIRED_APPIMAGE" "$RUNNING_APPIMAGE" >/dev/null 2>&1 </dev/null &
  else
    sh -c "$watchdog" wr-migration \
      "$LEGACY_BIN" "$RETIRED_APPIMAGE" "$RUNNING_APPIMAGE" >/dev/null 2>&1 </dev/null &
  fi
  log "Closing the retired AppImage."
}

require_flatpak

if migrating_from_appimage; then
  migrating=yes
  # Keep this marker shape compatible with the final AppImage updater's legacy
  # parser. It describes a migration phase, not a new AppImage download.
  log "Downloading WarcraftRecorder.AppImage migration instructions..."
else
  migrating=no
  log "Installing Warcraft Recorder..."
fi

ensure_runtime_source

if ! flatpak remote-add --user --if-not-exists "$REMOTE_NAME" "$REMOTE_DESCRIPTOR"; then
  fail_with_manual_instructions
fi

# Re-running this script is the documented way to update, and an install
# command fails on an application that is already there — including one that
# came from a bundle, whose origin is not this remote.
if flatpak info --user "$APP_REF" >/dev/null 2>&1; then
  origin=$(flatpak info --user --show-origin "$APP_REF" 2>/dev/null || true)
  if [[ "$origin" == "$REMOTE_NAME" ]]; then
    log "Warcraft Recorder is already installed; updating it."
    if ! flatpak update --user --assumeyes "$APP_REF"; then
      fail_with_manual_instructions
    fi
  else
    log "Replacing the existing install with the signed project release."
    if ! flatpak install --user --assumeyes --reinstall "$REMOTE_NAME" "$APP_REF"; then
      fail_with_manual_instructions
    fi
  fi
elif ! flatpak install --user --assumeyes "$REMOTE_NAME" "$APP_REF"; then
  fail_with_manual_instructions
fi

if [[ "$migrating" == yes ]]; then
  log "Checksum verified (Flatpak remote signature verified by Flatpak)."
  log "Installed binary: Flatpak $APP_ID"
  remove_appimage_install
fi

# Report session problems here rather than earlier: these do not block the
# install, and next to the launch is where they are still on screen.
check_session

# Start the app here. stdio goes to /dev/null: the AppImage updater waits for
# this script's pipes to close before it reports success.
log "Launching Warcraft Recorder..."
if command -v setsid >/dev/null 2>&1; then
  setsid flatpak run "$APP_REF" >/dev/null 2>&1 </dev/null &
else
  flatpak run "$APP_REF" >/dev/null 2>&1 </dev/null &
fi

if [[ "$migrating" == yes ]]; then
  close_running_appimage
  log "Done. Warcraft Recorder is now a Flatpak and updates through it."
else
  log "Done. Warcraft Recorder is installed and updates through Flatpak."
fi
