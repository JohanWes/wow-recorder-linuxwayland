# AUR packaging (warcraft-recorder-linux)

This fork ships Linux builds as an AppImage and supports publishing an AUR package that installs that AppImage system-wide.

## What users get

- Install/update via an AUR helper (examples):
  - `paru -S warcraft-recorder-linux`
  - `yay -S warcraft-recorder-linux`
- CLI entrypoints:
  - `warcraft-recorder-linux`
  - `warcraftrecorder_linux` (symlink)

## How nightly updates work

The AUR `PKGBUILD` downloads the AppImage from this repo’s GitHub Release named `nightly`:

- Tag name: `nightly`
- Asset name: `WarcraftRecorder-nightly.AppImage`

The workflow `.github/workflows/nightly-linux-appimage-and-aur.yml` updates both the `nightly` release asset and the AUR `PKGBUILD`/`.SRCINFO` on every push to `main`.

## Maintainer setup (one-time)

1. Create the AUR package repo named `warcraft-recorder-linux` (or set a different name via the secret below).
2. Create an SSH key that has write access to the AUR git repo.
3. Add GitHub Actions secrets in this fork:
   - `AUR_SSH_PRIVATE_KEY`: private key used to `git push` to AUR.
   - Optional: `AUR_PKGNAME`: defaults to `warcraft-recorder-linux`.

## Maintainer flow (ongoing)

1. Ensure `main` is updated (this fork already auto-rebases upstream daily).
2. Every push to `main`, the workflow will:
   - build the AppImage via `npm run package:linux`
   - upload `release/build/WarcraftRecorder-nightly.AppImage` to the `nightly` Release
   - render an AUR `PKGBUILD` + `.SRCINFO` and push to AUR

## Notes

- The wrapper supports `WARCRAFTRECORDER_NO_SANDBOX=1` to add `--no-sandbox` at launch.
- Runtime dependencies like `gpu-screen-recorder` / portals are listed as `optdepends` in the AUR package.
- This is a high-frequency AUR update style (nightly/commit-based). It’s ideal for this fork, but it may be noisy compared to release-based packaging.
