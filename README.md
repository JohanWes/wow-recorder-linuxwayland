# Warcraft Recorder

Warcraft Recorder is a native Rust/GTK4 application for Linux/Wayland that
tails the World of Warcraft combat log, captures activities with
`gpu-screen-recorder`, and keeps a local video library with combat metadata,
timeline markers, playback controls, clipping, and local POV switching.

The supported application ID is `io.github.JohanWes.WarcraftRecorder`. English
is the only shipped language and recordings/configuration remain local.

## Install and update

The normal install and update path is the project’s signed Flatpak remote:

```sh
flatpak remote-add --user --if-not-exists warcraft-recorder \
  https://johanwes.github.io/wow-recorder-linuxwayland/index.flatpakrepo
flatpak install --user warcraft-recorder io.github.JohanWes.WarcraftRecorder
```

After installation, the desktop software center or this command owns updates:

```sh
flatpak update --user io.github.JohanWes.WarcraftRecorder
```

The permanent remote is signed and published manually once a candidate is
approved. Until then, use the release-candidate bundle attached to the
candidate workflow:

```sh
flatpak install --user ./warcraft-recorder.flatpak
```

A bundle is an offline test artifact and does not configure the update remote.

Existing AppImage users should keep their final AppImage until the stable
Flatpak is announced. The migration release imports the existing legacy config
once without changing it; the native app then writes its own config and keeps
the existing recordings and sidecars in place. Rollback is launching that
untouched final AppImage again.

To uninstall the application while retaining user data:

```sh
flatpak uninstall --user io.github.JohanWes.WarcraftRecorder
```

Only pass `--delete-data` when deleting the native app’s private config and
runtime data is intentional. Recordings and legacy configuration are outside
that private directory and are not removed by app uninstall.

## Development

The repository contains one Cargo package under `native/`. Build and verify
it from the repository root:

```sh
cargo fmt --manifest-path native/Cargo.toml --check
cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path native/Cargo.toml --all-targets
cargo build --manifest-path native/Cargo.toml --release
```

The development Flatpak is
`flatpak/io.github.JohanWes.WarcraftRecorder.Devel.yml`; the release manifest
is `flatpak/io.github.JohanWes.WarcraftRecorder.yml`. Both use GNOME runtime
50 and the locked Cargo sources. See
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) for the native workflow and
[`docs/RELEASING.md`](docs/RELEASING.md) for candidate/signing steps.

## Scope and license

This fork is Linux/Wayland-only. Cloud/account/upload/chat/pro features,
localization catalogs, and other platform integrations are not part of the
native application. The native rewrite is licensed under
GPL-3.0-or-later. The capture engine is
[`gpu-screen-recorder`](https://git.dec05eba.com/gpu-screen-recorder/); the
original Warcraft Recorder is prior art.
