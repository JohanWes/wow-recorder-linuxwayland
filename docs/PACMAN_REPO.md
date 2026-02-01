# Pacman repo (GitHub Pages)

Because AUR registrations are currently disabled, this fork can be distributed to Arch/CachyOS users via a self-hosted pacman repository.

## User install (CachyOS / Arch)

1. Enable GitHub Pages for this repo (see maintainer section below).
2. Add this repo to `/etc/pacman.conf`:

```ini
[warcraft-recorder-linux]
SigLevel = Optional TrustAll
Server = https://<YOUR_GITHUB_USERNAME>.github.io/<YOUR_REPO_NAME>/repo/$arch
```

3. Install/update:

```bash
sudo pacman -Sy
sudo pacman -S warcraft-recorder-linux
```

Launch:

- `warcraft-recorder-linux`
- `warcraftrecorder_linux`

## Maintainer setup

1. Create an empty `gh-pages` branch in the fork (once).
2. Enable GitHub Pages:
   - Source: `Deploy from a branch`
   - Branch: `gh-pages` (root)
3. Ensure Actions are enabled.

The workflow `.github/workflows/nightly-linux-pacman-repo.yml` will, on every push to `main` (and after the auto-rebase workflow completes), build a pacman package and publish it under `gh-pages/repo/x86_64`.

## Notes

- This package installs the AppImage system-wide under `/opt/warcraft-recorder-linux/`.
- The wrapper sets `WARCRAFTRECORDER_DISABLE_UPDATER=1` so the in-app updater does not fight pacman updates.

