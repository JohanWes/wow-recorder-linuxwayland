# Warcraft Recorder

Warcraft Recorder (Linux/Wayland fork) watches the World of Warcraft combat log and automatically records videos for “interesting” activities (arenas, raids, dungeons, etc).

## Install

This fork publishes Linux AppImages via GitHub Releases. Every push to `main` that passes CI creates a new release.

### AppImage

Download the latest AppImage:

```bash
curl -L https://github.com/JohanWes/wow-recorder-linuxwayland/releases/latest/download/WarcraftRecorder.AppImage -o WarcraftRecorder.AppImage
```

Make it executable and launch:

```bash
chmod +x WarcraftRecorder.AppImage
./WarcraftRecorder.AppImage
```

Optional checksum verification:

```bash
curl -L https://github.com/JohanWes/wow-recorder-linuxwayland/releases/latest/download/WarcraftRecorder.AppImage.sha256 -o WarcraftRecorder.AppImage.sha256
sha256sum -c WarcraftRecorder.AppImage.sha256
```

Updates:

- Download the newest AppImage from the latest GitHub Release.

### Troubleshooting

- If sandboxing causes issues on your system: `WARCRAFTRECORDER_NO_SANDBOX=1 ./WarcraftRecorder.AppImage`

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
