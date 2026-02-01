# Warcraft Recorder

Warcraft Recorder (Linux/Wayland fork) watches the World of Warcraft combat log and automatically records videos for “interesting” activities (arenas, raids, dungeons, etc).

## Supported Platforms

Linux (Wayland).

| WoW flavour | Support |
|---|---|
| Retail | Yes |
| MoP Classic | Yes |
| Classic Era | SoD Raids Only |

## Quick Start

1. Install a combat logging addon and enable Advanced Combat Logging when prompted:
   - Retail: SimpleCombatLogger ([CurseForge](https://www.curseforge.com/wow/addons/simplecombatlogger), [Wago](https://addons.wago.io/addons/simplecombatlogger)).
   - Classic / Classic Era: AutoCombatLogger ([CurseForge](https://www.curseforge.com/wow/addons/autocombatlogger), [Wago](https://addons.wago.io/addons/autocombatlogger)).
2. In Warcraft Recorder settings:
   - Choose a Storage Path for videos.
   - Set the WoW `Logs` folder for the flavour(s) you play.
3. Use the Test button with WoW running to validate recording end-to-end.

## How Linux Capture Works

This fork uses `gpu-screen-recorder` (GSR) and portals (PipeWire + XDG Desktop Portal).

- Capture is portal-based (PipeWire + XDG Desktop Portal). On first start (or after “Re-select Capture Target”), you must select the WoW window/monitor in the system share dialog.
- Recording is fully automatic (start/stop is driven by combat log activity detection).
- The “Replay buffer” is only used for pre-roll; full activities are recorded as regular recordings and are not limited by the buffer length.

## Prerequisites (Wayland)

Required on most Wayland setups:
- `gpu-screen-recorder`
- PipeWire
- `xdg-desktop-portal`
- A portal backend for your compositor/DE (e.g. `xdg-desktop-portal-hyprland`, `xdg-desktop-portal-gnome`, `xdg-desktop-portal-kde`, `xdg-desktop-portal-wlr`)

The app performs best-effort runtime checks and reports missing prerequisites via the in-app error indicator.

### CachyOS / Arch packages (example)

Install prerequisites (example):

- `sudo pacman -S gpu-screen-recorder pipewire xdg-desktop-portal fuse2`
- Portal backend (pick one): `sudo pacman -S xdg-desktop-portal-hyprland` (Hyprland) / `xdg-desktop-portal-kde` / `xdg-desktop-portal-gnome` / `xdg-desktop-portal-wlr`

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

If you don’t like piping to `bash`, do:

```bash
curl -fsSL https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/scripts/install-pacman-repo.sh -o /tmp/install-warcraft-recorder-linux.sh
sudo bash /tmp/install-warcraft-recorder-linux.sh
```

### Troubleshooting

- If sandboxing causes issues on your system: `WARCRAFTRECORDER_NO_SANDBOX=1 warcraft-recorder-linux`

## Alternative Install (AppImage)

You can also use the AppImage directly (e.g. from the `Nightly` release):

- `chmod +x WarcraftRecorder-*.AppImage`
- `./WarcraftRecorder-*.AppImage`
- If FUSE is missing: `./WarcraftRecorder-*.AppImage --appimage-extract-and-run`

## Building / Packaging (AppImage)

Linux packaging produces an AppImage:

- `npm install`
- `npm run package:linux`

Use Node 22 LTS (or Node 20 LTS) for packaging.

## Contributing

See `docs/CONTRIBUTING.md`.

## Credits

- Linux recording uses [gpu-screen-recorder](https://git.dec05eba.com/gpu-screen-recorder/about/).
- Built with [Electron](https://www.electronjs.org/) and [React](https://react.dev/) (ERB).
