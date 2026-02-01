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

collect_pacman_config_files() {
  # Walk /etc/pacman.conf and all transitive Include= globs to find every config file
  # that pacman will parse (best-effort).
  local -a queue out
  declare -A seen
  local f inc expanded

  queue=("${pacman_conf}")
  out=()

  while [[ ${#queue[@]} -gt 0 ]]; do
    f="${queue[0]}"
    queue=("${queue[@]:1}")

    [[ -f "${f}" ]] || continue
    if [[ -n "${seen["${f}"]+x}" ]]; then
      continue
    fi

    seen["${f}"]=1
    out+=("${f}")

    while IFS= read -r inc; do
      # Normalize: "Include = /path/*.conf" -> "/path/*.conf"
      inc="$(echo "${inc}" | sed -E 's/^[[:space:]]*Include[[:space:]]*=[[:space:]]*//')"
      inc="$(echo "${inc}" | sed -E 's/[[:space:]]+$//')"
      [[ -n "${inc}" ]] || continue

      # Expand globs; if none match, keep as-is (pacman would error, but we can ignore).
      shopt -s nullglob
      expanded=(${inc})
      shopt -u nullglob

      if [[ ${#expanded[@]} -eq 0 ]]; then
        expanded=("${inc}")
      fi

      for e in "${expanded[@]}"; do
        queue+=("${e}")
      done
    done < <(rg -n --no-messages '^[[:space:]]*Include[[:space:]]*=' "${f}" 2>/dev/null | sed -E 's/^[0-9]+://')
  done

  printf '%s\n' "${out[@]}"
}

find_repo_headers() {
  local pattern files
  pattern="^\[${repo_name}\]$"
  mapfile -t files < <(collect_pacman_config_files)
  if [[ ${#files[@]} -eq 0 ]]; then
    return 0
  fi
  rg -n --no-messages "${pattern}" "${files[@]}" 2>/dev/null || true
}

repo_header_count() {
  local matches
  matches="$(find_repo_headers)"
  printf '%s\n' "${matches}" | sed '/^$/d' | wc -l | awk '{print $1}'
}

repo_already_configured() {
  [[ "$(repo_header_count)" -ge 1 ]]
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
