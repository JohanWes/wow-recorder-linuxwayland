#!/usr/bin/env bash
set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
  echo "This installer must be run as root (use sudo)." >&2
  exit 1
fi

repo_name="${PACMAN_REPO_NAME:-warcraft-recorder-linux}"
repo_url="${PACMAN_REPO_URL:-https://github.com/JohanWes/wow-recorder-linuxwayland/releases/download/pacman}"
pkg_name="${PACMAN_PKG_NAME:-warcraft-recorder-linux}"

pacman_conf="/etc/pacman.conf"

if [[ ! -f "${pacman_conf}" ]]; then
  echo "Missing ${pacman_conf}" >&2
  exit 1
fi

if ! command -v pacman >/dev/null 2>&1; then
  echo "pacman not found. This installer is intended for Arch-based systems." >&2
  exit 1
fi

if command -v rg >/dev/null 2>&1; then
  :
else
  echo "ripgrep (rg) not found; falling back to grep." >&2
  rg() { grep -n "$@"; }
fi

repo_already_configured() {
  # Prefer pacman-conf since it sees Include'd config files too.
  if command -v pacman-conf >/dev/null 2>&1; then
    pacman-conf --repo-list 2>/dev/null | rg -xq "${repo_name}" && return 0
  fi

  rg -n "^[[]${repo_name}[]]$" "${pacman_conf}" >/dev/null 2>&1
}

ensure_repo_config() {
  if repo_already_configured; then
    return 0
  fi

  ts="$(date +%Y%m%d-%H%M%S)"
  cp -a "${pacman_conf}" "${pacman_conf}.bak-${ts}"

  cat >> "${pacman_conf}" <<EOF

[${repo_name}]
SigLevel = Optional TrustAll
Server = ${repo_url}
EOF
}

ensure_repo_config

pacman -Sy --needed "${pkg_name}"

echo "Installed: ${pkg_name}"
echo "Launch: warcraft-recorder-linux"
