# Warcraft Recorder

Warcraft Recorder records your World of Warcraft gameplay by itself. It watches
the combat log and saves a video for every raid pull, Mythic+ dungeon, arena,
solo shuffle and battleground, with a death timeline you can jump through,
clipping, slow motion, a local-POV switch and a detailed local combat meter.

Linux and Wayland only. Everything stays on your machine.

![Warcraft Recorder library with a selected Mythic+ recording](data/screenshots/warcraft-recorder-library.png)

## Combat review without leaving the recorder

The built-in meter shows damage done and taken, healing, interrupts, dispels,
casts and deaths alongside the video. Switch between the current fight and
overall data, filter by target, drill into each player's spells and jump to
the relevant moment in the recording.

For many reviews this can replace finding or uploading a report on Warcraft
Logs: the detailed combat data is already available inside the recorder and
stays on your machine.

![Damage meter drill-down, options, target filter and spell tooltip](data/screenshots/damage-meter-demo.gif)

## Install

One command in a terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/install.sh | bash
```

If your terminal says `curl` is missing, use the Flatpak commands below instead.

It installs the app from the project's signed Flatpak remote and starts it.
Run the same command again any time to update.

Prefer typing the commands yourself?

```sh
flatpak remote-add --user --if-not-exists warcraft-recorder \
  https://johanwes.github.io/wow-recorder-linuxwayland/index.flatpakrepo
flatpak install --user warcraft-recorder io.github.JohanWes.WarcraftRecorder
```

## What you need

- **A Wayland desktop session.** KDE Plasma, GNOME, Hyprland and COSMIC all
  work. X11 does not.
- **Flatpak.** Most gaming distributions ship it. If it is missing, the
  installer shows the command for common distributions (`sudo pacman -S
  flatpak`, `sudo dnf install flatpak`, `sudo apt install flatpak` or `sudo
  zypper install flatpak`).
- **Desktop screen sharing**, meaning `xdg-desktop-portal` and PipeWire. This
  is how a sandboxed app is allowed to see your screen, and every mainstream
  desktop already sets it up. A hand-assembled Hyprland or wlroots session may
  still need its own portal package.
- **A GPU with hardware video encoding**, which most AMD, Intel and NVIDIA
  cards from the last decade have.

If your session is missing Wayland, the portal or PipeWire, the installer says
so.

The recorder (`gpu-screen-recorder`), the video player and FFmpeg are all
inside the Flatpak, so there is nothing else to install and nothing to keep in
sync. Flatpak also downloads the shared GNOME runtime if you do not already
have it: that is a set of libraries, not the GNOME desktop.

## First run

1. In Settings, pick your **recording folder** and your **World of Warcraft
   Logs folder** (for example
   `.../World of Warcraft/_retail_/Logs`). The app highlights both until they
   are set.
2. In WoW, turn on **Advanced Combat Logging** (Escape → Options → System →
   Network), and use an addon such as
   [SimpleCombatLogger](https://www.curseforge.com/wow/addons/simplecombatlogger)
   so logging starts on its own when you enter an instance.
3. Play. Each activity shows up in the library when it ends. Closing the window
   keeps recording from the tray icon, so long as your desktop has a tray:
   GNOME needs the
   [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/)
   for one, and without it, closing the window quits the app.

## Update and uninstall

Updates arrive through your software centre, or:

```sh
flatpak update --user io.github.JohanWes.WarcraftRecorder
```

To remove the app while keeping your videos and settings:

```sh
flatpak uninstall --user io.github.JohanWes.WarcraftRecorder
```

Your recordings are ordinary files in your recording folder and are never
touched by an uninstall.

## Why the rewrite

This version is written in Rust with GTK4 instead of Electron. Measured on the
same machine, with the app open and idle on an empty library:

| | This version | Old Electron version |
|---|---|---|
| Install | 20 MB | 187 MB |
| Memory (idle) | ~150 MB | ~650 MB |
| Application processes | 1 | 7 |
| CPU (idle) | ~0% | ~0% |

The whole application is a single 7.8 MB binary built on eleven direct
dependencies, with no browser engine, no database and no background services.
Recording itself costs almost nothing either way: `gpu-screen-recorder` encodes
on the GPU, exactly as it did before.

## Coming from the AppImage

Press "Check for updates" in the old app, or run the install command above. The
AppImage, its launcher and its menu entry are removed, your settings are
imported once, and your recordings, tags and protected videos stay exactly
where they are. You will be asked to pick the recording and Logs folders again,
because the sandboxed app cannot see them until you do.

## Development

One Cargo package under `native/`. From the repository root:

```sh
cargo fmt --manifest-path native/Cargo.toml --check
cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path native/Cargo.toml --all-targets
cargo build --manifest-path native/Cargo.toml --release
```

See [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) for the workflow and
[`docs/RELEASING.md`](docs/RELEASING.md) for candidate and signing steps.

## Scope and license

This fork is Linux/Wayland-only and English-only; cloud, account and upload
features are not part of it. Licensed GPL-3.0-or-later. Capture is
[`gpu-screen-recorder`](https://git.dec05eba.com/gpu-screen-recorder/); the
original [Warcraft Recorder](https://github.com/aza547/wow-recorder) is prior
art.
