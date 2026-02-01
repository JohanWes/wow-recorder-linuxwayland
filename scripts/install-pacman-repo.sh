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

find_repo_headers() {
  # Return matching lines (with file/line) across common pacman config locations.
  # This helps diagnose "database already registered" (repo defined multiple times).
  local pattern
  pattern="^[[]${repo_name}[]]$"

  if [[ -d /etc/pacman.d ]]; then
    rg -n --no-messages "${pattern}" "${pacman_conf}" /etc/pacman.d 2>/dev/null || true
  else
    rg -n --no-messages "${pattern}" "${pacman_conf}" 2>/dev/null || true
  fi
}

fail_if_duplicate_repo_headers() {
  local matches count
  matches="$(find_repo_headers)"
  count="$(printf '%s\n' "${matches}" | sed '/^$/d' | wc -l | awk '{print $1}')"

  if [[ "${count}" -gt 1 ]]; then
    echo "Repo [${repo_name}] is configured more than once. Remove duplicates, then retry." >&2
    echo "Found entries at:" >&2
    printf '%s\n' "${matches}" >&2
    exit 1
  fi
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

fail_if_duplicate_repo_headers
ensure_repo_config

pacman -Sy --needed "${pkg_name}"

echo "Installed: ${pkg_name}"
echo "Launch: warcraft-recorder-linux"
