# Warcraft Recorder

Warcraft Recorder (Linux/Wayland fork) watches the World of Warcraft combat log and automatically records videos for “interesting” activities (arenas, raids, dungeons, etc).

## Install

This fork publishes Linux AppImages via GitHub Releases. Every push to `main` that passes CI creates a new release.

### Option 1: Installer script (recommended)

Run the installer. It downloads the latest AppImage, verifies the SHA256 checksum, and installs it:

```bash
curl -fsSL https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/install.sh | bash
```

By default it installs per-user into:

- `~/.local/bin/warcraftrecorder`
- `~/.local/share/applications/warcraftrecorder.desktop`
- `~/.local/share/icons/hicolor/256x256/apps/warcraftrecorder.png`

Run it from the terminal or launch **Warcraft Recorder** from your application menu:

```bash
warcraftrecorder
```

If `~/.local/bin` is not on your `PATH`, the installer will tell you how to add it.

#### Updating

Re-run the same command. The installer overwrites any existing install with the latest release.

#### Uninstall

```bash
rm ~/.local/bin/warcraftrecorder \
   ~/.local/share/applications/warcraftrecorder.desktop \
   ~/.local/share/icons/hicolor/256x256/apps/warcraftrecorder.png
```

#### Installer flags

```bash
curl -fsSL https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/install.sh | bash -s -- --no-desktop
```

Available flags:

- `--prefix <dir>` — install under a custom writable prefix (`bin`, `share/applications`, and `share/icons` are created there).
- `--no-desktop` — skip creating the application menu entry.
- `--no-verify` — skip SHA256 checksum verification.
- `--use-sudo` — use `sudo` for install steps when `--prefix` points to a system location such as `/opt/warcraftrecorder`.
- `--tag <tag>` — install a specific release tag instead of `latest`.
- `--repo <owner/repo>` — install from a different fork or mirror.

System-wide installs need write permission for the target prefix. For example:

```bash
curl -fsSL https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/install.sh | bash -s -- --prefix /opt/warcraftrecorder --no-desktop --use-sudo
```

### Option 2: Manual AppImage install

If you prefer not to run a remote script, download the AppImage directly from the [latest release](https://github.com/JohanWes/wow-recorder-linuxwayland/releases/latest):

```bash
curl -LO https://github.com/JohanWes/wow-recorder-linuxwayland/releases/latest/download/WarcraftRecorder.AppImage
chmod +x WarcraftRecorder.AppImage
./WarcraftRecorder.AppImage
```

Put it anywhere in your `PATH` and rename it if you want a quick command:

```bash
mv WarcraftRecorder.AppImage ~/.local/bin/warcraftrecorder
```

Updates are manual: download the newest AppImage and replace your existing file.

## Dependencies / Prerequisites (Wayland)

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
