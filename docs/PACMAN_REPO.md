# Pacman repo (GitHub Releases)

Because AUR registrations are currently disabled, this fork can be distributed to Arch/CachyOS users via a self-hosted pacman repository.

## User install (CachyOS / Arch)

Add this repo to `/etc/pacman.conf`:

```ini
[warcraft-recorder-linux]
SigLevel = Optional TrustAll
Server = https://github.com/<YOUR_GITHUB_USERNAME>/<YOUR_REPO_NAME>/releases/download/pacman
```

Install/update:

```bash
sudo pacman -Sy
sudo pacman -S warcraft-recorder-linux
```

Launch:

- `warcraft-recorder-linux`
- `warcraftrecorder_linux`

## Maintainer setup

1. Ensure Actions are enabled.
2. Ensure the repository is public (pacman clients must fetch assets anonymously).

The workflow `.github/workflows/nightly-linux-pacman-repo.yml` will, on every push to `main` (and after the auto-rebase workflow completes), build:

- a pacman package (`warcraft-recorder-linux-*.pkg.tar.zst`)
- repo db files (`warcraft-recorder-linux.db` / `warcraft-recorder-linux.files`)

…and upload them to a GitHub Release tag named `pacman`.

## Notes

- This package installs the AppImage system-wide under `/opt/warcraft-recorder-linux/`.
- The wrapper sets `WARCRAFTRECORDER_DISABLE_UPDATER=1` so the in-app updater does not fight pacman updates.
- GitHub Pages cannot host these packages because GitHub rejects pushes of files larger than 100MB; GitHub Releases supports larger artifacts.
