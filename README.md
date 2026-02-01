# Warcraft Recorder

Warcraft Recorder (Linux/Wayland fork) watches the World of Warcraft combat log and automatically records videos for “interesting” activities (arenas, raids, dungeons, etc).

## Install (CachyOS / Arch via pacman repo)

This fork publishes a pacman repository via GitHub Releases so you can install/update like any other package.

### Manual (recommended)

1. Add the repo to `/etc/pacman.conf` (one-time):

```ini
[warcraft-recorder-linux]
SigLevel = Optional TrustAll
Server = https://github.com/JohanWes/wow-recorder-linuxwayland/releases/download/pacman
```

2. Sync + install:

```bash
sudo pacman -Sy warcraft-recorder-linux
```

3. Launch:

```bash
warcraft-recorder-linux
```

Updates:

- `sudo pacman -Syu`

### One-command install (bootstrap)

If you prefer, this script will append the repo to `/etc/pacman.conf` (with a timestamped backup) and install the package:

```bash
curl -fsSL https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/scripts/install-pacman-repo.sh | sudo bash
```

If you see `database already registered`, you have the repo configured more than once (remove duplicates in pacman config and re-run).

If you don’t like piping to `bash`, do:

```bash
curl -fsSL https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/scripts/install-pacman-repo.sh -o /tmp/install-warcraft-recorder-linux.sh
sudo bash /tmp/install-warcraft-recorder-linux.sh
```

### Troubleshooting

- If sandboxing causes issues on your system: `WARCRAFTRECORDER_NO_SANDBOX=1 warcraft-recorder-linux`

## Dependencies / Prerequisites (Wayland)

When installing via pacman, these are installed automatically as dependencies:

- `gpu-screen-recorder`
- `pipewire`
- `xdg-desktop-portal`
- `fuse2`

You still need one portal backend for your compositor/DE (install one of):

- `xdg-desktop-portal-hyprland` (Hyprland)
- `xdg-desktop-portal-kde` (KDE)
- `xdg-desktop-portal-gnome` (GNOME)
- `xdg-desktop-portal-wlr` (wlroots)

The app performs best-effort runtime checks and reports missing prerequisites via the in-app error indicator.

## Quick Start

1. Install a combat logging addon and enable Advanced Combat Logging when prompted:
   - Retail: SimpleCombatLogger ([CurseForge](https://www.curseforge.com/wow/addons/simplecombatlogger), [Wago](https://addons.wago.io/addons/simplecombatlogger)).
   - Classic / Classic Era: AutoCombatLogger ([CurseForge](https://www.curseforge.com/wow/addons/autocombatlogger), [Wago](https://addons.wago.io/addons/autocombatlogger)).
2. In Warcraft Recorder settings:
   - Choose a Storage Path for videos.
   - Set the WoW `Logs` folder for the flavour(s) you play.
3. Use the Test button with WoW running to validate recording end-to-end.

## Building / Packaging (AppImage)

Linux packaging produces an AppImage:

- `npm install`
- `npm run package:linux`

Use Node 22 LTS (or Node 20 LTS) for packaging.
