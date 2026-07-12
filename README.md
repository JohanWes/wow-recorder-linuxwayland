# Warcraft Recorder

Warcraft Recorder is a Linux/Wayland Tauri app that watches the World of Warcraft retail combat log and records gameplay videos for supported activities.

## Prerequisites

- Node.js 22
- Rust stable
- Tauri's Linux system dependencies
- `gpu-screen-recorder`
- `ffmpeg`
- PipeWire and an `xdg-desktop-portal` backend for your desktop environment

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/install.sh | bash
```

The installer downloads the latest AppImage, verifies its SHA256 checksum, and
checks that `gpu-screen-recorder` and `ffmpeg` are available.

## Development

```bash
npm install
npm run tauri dev
```

Build the desktop application with:

```bash
npm run tauri build
```

The build disables `linuxdeploy`'s bundled `strip`, which is incompatible with
newer RELR-enabled Linux system libraries. Rust still strips the application
binary in its release profile.

Frontend-only checks are available through `npm run typecheck`, `npm run build`, and `npm run lint`. Backend checks run from `src-tauri` with `cargo check` and `cargo test`.
