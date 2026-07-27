# Changelog

Notable changes to the native Linux/Wayland application. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/) and
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Release history before the native rewrite belongs to the upstream Electron
project, [aza547/wow-recorder](https://github.com/aza547/wow-recorder).

## Unreleased

### Added
- Native Rust/GTK4 application: combat-log watching, activity detection,
  `gpu-screen-recorder` capture, JSON sidecar library, playback with a combat
  timeline, clipping, drawing overlay, and local POV switching.
- Live keystone timers from the API when computing a Mythic+ result, with
  hardcoded timers as a fallback.

### Changed
- Flatpak is the only install and update path. Recordings and configuration
  stay local; legacy configuration is imported once and left untouched.
- Cloud, account, upload, and localization features are not part of this fork.
