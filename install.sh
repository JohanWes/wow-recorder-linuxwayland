#!/usr/bin/env bash
set -euo pipefail

# curl|bash installer for the Warcraft Recorder Linux AppImage.
# Re-running the script overwrites an existing install with the latest release.

REPO="${WARCRAFTRECORDER_REPO:-JohanWes/wow-recorder-linuxwayland}"
APP_NAME="warcraftrecorder"
DISPLAY_NAME="Warcraft Recorder"
APPIMAGE_NAME="WarcraftRecorder.AppImage"
CHECKSUM_NAME="${APPIMAGE_NAME}.sha256"
ICON_NAME="warcraftrecorder.png"
DESKTOP_NAME="warcraftrecorder.desktop"

PREFIX="${HOME}/.local"
NO_DESKTOP=0
NO_VERIFY=0
USE_SUDO=0
RELEASE_TAG="latest"

bin_dir=""
share_dir=""
desktop_dir=""
icon_dir=""
metadata_dir=""
SUDO=()
INSTALLED_RELEASE_TAG=""

usage() {
  cat <<EOF
Install ${DISPLAY_NAME} from the latest GitHub release.

Usage:
  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | bash
  bash install.sh [OPTIONS]

Options:
  --prefix <dir>   Install prefix (default: ${PREFIX})
  --no-desktop     Skip creating the application menu entry
  --no-verify      Skip SHA256 checksum verification
  --use-sudo       Use sudo for install steps when the prefix is not writable
  --repo <o/r>     Use a different GitHub repository (default: ${REPO})
  --tag <tag>      Install a specific release tag (default: latest)
  -h, --help       Show this help message

Environment:
  WARCRAFTRECORDER_REPO   Default repository override (owner/repo)
EOF
}

log() { printf '[install] %s\n' "$*"; }
warn() { printf '[install] WARNING: %s\n' "$*" >&2; }
error() { printf '[install] ERROR: %s\n' "$*" >&2; exit 1; }

run_privileged() {
  if [ "${#SUDO[@]}" -gt 0 ]; then
    "${SUDO[@]}" "$@"
  else
    "$@"
  fi
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --prefix)
        [ -n "${2:-}" ] || error "--prefix requires an argument"
        PREFIX="$2"
        shift 2
        ;;
      --no-desktop) NO_DESKTOP=1; shift ;;
      --no-verify) NO_VERIFY=1; shift ;;
      --use-sudo) USE_SUDO=1; shift ;;
      --repo)
        [ -n "${2:-}" ] || error "--repo requires an argument"
        REPO="$2"
        shift 2
        ;;
      --tag)
        [ -n "${2:-}" ] || error "--tag requires an argument"
        RELEASE_TAG="$2"
        shift 2
        ;;
      -h|--help) usage; exit 0 ;;
      *) error "Unknown argument: $1" ;;
    esac
  done
}

download() {
  local url dest
  url="$1"
  dest="$2"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --retry-delay 2 -o "$dest" "$url" || error "failed to download ${url}"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$dest" "$url" || error "failed to download ${url}"
  else
    error "curl or wget is required"
  fi
}

check_dependencies() {
  if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    error "curl or wget is required"
  fi

  if [ "$NO_VERIFY" -eq 0 ] && ! command -v sha256sum >/dev/null 2>&1; then
    error "sha256sum is required for verification (use --no-verify to skip)"
  fi
}

warn_missing_fuse() {
  # AppImage needs libfuse2 in most cases.
  if ! ldconfig -p 2>/dev/null | grep -q 'libfuse\.so\.2' 2>/dev/null; then
    warn "libfuse2 (fuse2) does not appear to be installed."
    warn "The AppImage may fail to launch until you install libfuse2 / fuse2."
  fi
}

resolve_release_urls() {
  local release_root
  if [ "$RELEASE_TAG" = "latest" ]; then
    release_root="https://github.com/${REPO}/releases/latest/download"
  else
    release_root="https://github.com/${REPO}/releases/download/${RELEASE_TAG}"
  fi
  APPIMAGE_URL="${release_root}/${APPIMAGE_NAME}"
  CHECKSUM_URL="${release_root}/${CHECKSUM_NAME}"
  ICON_URL="https://raw.githubusercontent.com/${REPO}/main/assets/icon.png"
}

resolve_latest_release_tag() {
  local location

  [ "$RELEASE_TAG" = "latest" ] || return 0
  command -v curl >/dev/null 2>&1 || return 0

  location=$(curl -fsSI "$APPIMAGE_URL" | awk 'BEGIN { IGNORECASE=1 } /^location:/ { sub(/\r$/, "", $2); print $2; exit }') || return 0
  INSTALLED_RELEASE_TAG=$(release_tag_from_url "$location")
  if [ -n "$INSTALLED_RELEASE_TAG" ]; then
    log "Resolved latest release: ${INSTALLED_RELEASE_TAG}"
  fi
}

compute_dirs() {
  bin_dir="${PREFIX}/bin"
  share_dir="${PREFIX}/share"
  desktop_dir="${share_dir}/applications"
  icon_dir="${share_dir}/icons/hicolor/256x256/apps"
  metadata_dir="${share_dir}/${APP_NAME}"
}

nearest_existing_parent() {
  local path
  path="$1"

  while [ ! -e "$path" ]; do
    path=$(dirname "$path")
  done

  printf '%s\n' "$path"
}

prefix_is_writable() {
  local parent

  if [ -d "$PREFIX" ]; then
    [ -w "$PREFIX" ]
    return
  fi

  parent=$(nearest_existing_parent "$PREFIX")
  [ -d "$parent" ] && [ -w "$parent" ]
}

setup_privilege() {
  if [ "$(id -u)" -eq 0 ] || prefix_is_writable; then
    SUDO=()
    return
  fi

  if [ "$USE_SUDO" -eq 0 ]; then
    error "prefix ${PREFIX} is not writable by the current user. Use the default user install, choose a writable --prefix, or pass --use-sudo for a system install."
  fi

  if ! command -v sudo >/dev/null 2>&1; then
    error "prefix ${PREFIX} is not writable and sudo is not available. Choose a user-writable --prefix."
  fi

  log "Prefix ${PREFIX} is not writable by the current user; using sudo for install steps."
  sudo -v || error "sudo authentication failed"
  SUDO=(sudo)
}

release_tag_from_url() {
  local url tag
  url="$1"

  case "$url" in
    */releases/download/*/*)
      tag="${url#*/releases/download/}"
      tag="${tag%%/*}"
      printf '%s\n' "$tag"
      ;;
  esac
}

release_tag_to_install() {
  if [ -n "$INSTALLED_RELEASE_TAG" ]; then
    printf '%s\n' "$INSTALLED_RELEASE_TAG"
    return
  fi

  if [ "$RELEASE_TAG" != "latest" ]; then
    printf '%s\n' "$RELEASE_TAG"
  fi
}

check_runtime_dependencies() {
  local package_manager command binary answer

  if command -v pacman >/dev/null 2>&1; then
    package_manager="pacman"
  elif command -v apt-get >/dev/null 2>&1; then
    package_manager="apt-get"
  elif command -v dnf >/dev/null 2>&1; then
    package_manager="dnf"
  elif command -v zypper >/dev/null 2>&1; then
    package_manager="zypper"
  else
    package_manager=""
  fi

  for binary in gpu-screen-recorder ffmpeg; do
    command -v "$binary" >/dev/null 2>&1 && continue

    case "$package_manager" in
      pacman) command="sudo pacman -S --needed ${binary}" ;;
      apt-get) command="sudo apt-get install ${binary}" ;;
      dnf) command="sudo dnf install ${binary}" ;;
      zypper) command="sudo zypper install ${binary}" ;;
      *)
        warn "${binary} is required at runtime but was not found. Install it with your distribution's package manager."
        continue
        ;;
    esac

    if [ -t 0 ]; then
      printf '[install] %s is required at runtime but was not found. Install it now with "%s"? [y/N] ' "$binary" "$command"
      read -r answer
      case "$answer" in
        y|Y|yes|YES) bash -c "$command" || warn "failed to install ${binary}; run: ${command}" ;;
        *) warn "${binary} is still missing. Install it with: ${command}" ;;
      esac
    else
      warn "${binary} is required at runtime but was not found. Install it with: ${command}"
    fi
  done
}

main() {
  parse_args "$@"
  resolve_release_urls
  compute_dirs
  check_dependencies
  setup_privilege

  local tmp_dir
  tmp_dir=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '${tmp_dir}'" EXIT

  log "Installing ${DISPLAY_NAME} from ${REPO} release ${RELEASE_TAG}..."
  resolve_latest_release_tag

  run_privileged mkdir -p "$bin_dir" "$metadata_dir"
  if [ "$NO_DESKTOP" -eq 0 ]; then
    run_privileged mkdir -p "$desktop_dir" "$icon_dir"
  fi

  log "Downloading ${APPIMAGE_NAME}..."
  download "$APPIMAGE_URL" "${tmp_dir}/${APPIMAGE_NAME}"

  if [ "$NO_VERIFY" -eq 0 ]; then
    log "Downloading ${CHECKSUM_NAME}..."
    download "$CHECKSUM_URL" "${tmp_dir}/${CHECKSUM_NAME}"

    local expected actual
    expected=$(awk '{print $1}' "${tmp_dir}/${CHECKSUM_NAME}")
    actual=$(sha256sum "${tmp_dir}/${APPIMAGE_NAME}" | awk '{print $1}')

    if [ "$expected" != "$actual" ]; then
      error "SHA256 mismatch: expected ${expected}, got ${actual}"
    fi
    log "Checksum verified (${actual})."
  else
    warn "skipping SHA256 verification"
  fi

  local dest_appimage dest_desktop dest_icon
  dest_appimage="${bin_dir}/${APP_NAME}"
  dest_desktop="${desktop_dir}/${DESKTOP_NAME}"
  dest_icon="${icon_dir}/${ICON_NAME}"

  # Remove any previous install so the overwrite is clean.
  run_privileged rm -f "$dest_appimage"

  run_privileged install -m 755 "${tmp_dir}/${APPIMAGE_NAME}" "$dest_appimage"
  log "Installed binary: ${dest_appimage}"
  check_runtime_dependencies

  local installed_tag
  installed_tag=$(release_tag_to_install)
  if [ -n "$installed_tag" ]; then
    printf '%s\n' "$installed_tag" > "${tmp_dir}/release-tag"
    run_privileged install -m 644 "${tmp_dir}/release-tag" "${metadata_dir}/release-tag"
    log "Installed release metadata: ${metadata_dir}/release-tag"
  fi

  if [ "$NO_DESKTOP" -eq 0 ]; then
    log "Downloading icon..."
    download "$ICON_URL" "${tmp_dir}/${ICON_NAME}" || warn "icon download failed; desktop entry may lack an icon"

    run_privileged install -m 644 "${tmp_dir}/${ICON_NAME}" "$dest_icon" 2>/dev/null || true
    log "Installed icon: ${dest_icon}"

    local tmp_desktop
    tmp_desktop="${tmp_dir}/${DESKTOP_NAME}"
    cat > "$tmp_desktop" <<EOF
[Desktop Entry]
Name=${DISPLAY_NAME}
Comment=World of Warcraft combat log recorder
Exec=${dest_appimage} %U
Icon=${dest_icon}
Type=Application
Terminal=false
StartupNotify=true
Categories=AudioVideo;Video;Recorder;
TryExec=${dest_appimage}
EOF
    run_privileged install -m 644 "$tmp_desktop" "$dest_desktop"
    log "Installed desktop entry: ${dest_desktop}"
  fi

  if ! command -v "$APP_NAME" >/dev/null 2>&1; then
    case ":${PATH}:" in
      *:"${bin_dir}":*) ;;
      *)
        warn "${bin_dir} is not on your PATH."
        printf 'Add this to your ~/.bashrc or ~/.zshrc if you want to run "%s" from anywhere:\n' "$APP_NAME"
        # shellcheck disable=SC2016
        printf '  export PATH="%s:$PATH"\n' "$bin_dir"
        ;;
    esac
  fi

  warn_missing_fuse
  log "Done. Run '${APP_NAME}' or launch ${DISPLAY_NAME} from your application menu."
}

main "$@"
