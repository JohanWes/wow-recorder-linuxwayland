#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 7 ]]; then
  echo "usage: $0 <pkgver> <github_owner> <github_repo> <appimage_sha256> <icon_sha256> <out_dir> <appimage_path>" >&2
  exit 2
fi

pkgver="$1"
github_owner="$2"
github_repo="$3"
appimage_sha256="$4"
icon_sha256="$5"
out_dir="$6"
appimage_path="$7"

template_dir="packaging/pacman/warcraft-recorder-linux"

mkdir -p "${out_dir}"

cp -a "${template_dir}/warcraft-recorder-linux.desktop" "${out_dir}/warcraft-recorder-linux.desktop"
cp -a "${template_dir}/warcraft-recorder-linux.sh" "${out_dir}/warcraft-recorder-linux.sh"
cp -a "assets/icon.png" "${out_dir}/icon.png"
cp -a "${appimage_path}" "${out_dir}/WarcraftRecorder.AppImage"

desktop_sha256="$(sha256sum "${out_dir}/warcraft-recorder-linux.desktop" | awk '{print $1}')"
wrapper_sha256="$(sha256sum "${out_dir}/warcraft-recorder-linux.sh" | awk '{print $1}')"

sed \
  -e "s|@PKGVER@|${pkgver}|g" \
  -e "s|@GITHUB_OWNER@|${github_owner}|g" \
  -e "s|@GITHUB_REPO@|${github_repo}|g" \
  -e "s|@APPIMAGE_SHA256@|${appimage_sha256}|g" \
  -e "s|@DESKTOP_SHA256@|${desktop_sha256}|g" \
  -e "s|@WRAPPER_SHA256@|${wrapper_sha256}|g" \
  -e "s|@ICON_SHA256@|${icon_sha256}|g" \
  "${template_dir}/PKGBUILD.tmpl" > "${out_dir}/PKGBUILD"

echo "Wrote: ${out_dir}/PKGBUILD"

