# WR-014: Flatpak release candidate, pipeline, and permanent remote

## Goal

Turn the proven development Flatpak into a reproducible release build with one permanent user-installable Flatpak remote (a signed static OSTree repo, e.g. GitHub Pages) and the smallest tag-triggered pipeline. Nothing is published to the permanent remote before WR-015 passes.

## Dependencies

WR-002, WR-010, WR-011, and WR-012 must be `DONE`. WR-013 consumes this ticket's release-candidate build and permanent remote URL; WR-015 alone authorizes publication.

## Owned files

- release Flatpak manifest and shared module fragments under `flatpak/`
- `flatpak/cargo-sources.json`
- AppStream/desktop/icon metadata under `data/`
- release CI workflow/scripts
- root installation/release documentation
- `implementation_docs/reports/wr-014-release.md`

## Release manifest

1. Derive the release manifest from the working Devel manifest with the stable app ID, release command, metadata, and only the permissions proven in WR-002/WR-012. Share a small YAML fragment only if the existing tooling supports it cleanly; duplicating a short manifest is preferable to adding a templating framework.
2. Pin GNOME runtime/SDK, Rust sources, GSR, Clapper/ClapperGtk, minimal FFmpeg build, and every external archive/commit with immutable URL plus checksum/commit. Cargo builds offline from locked sources.
3. Strip debug artifacts and development probe entry points. Do not strip required licenses, GStreamer plugins, codecs, assets, or GSR/FFmpeg features proven by parity.
4. Validate app ID, command, icon names, categories, screenshots, license, releases, URLs, and English AppStream copy. No localization catalog is shipped.
5. Document how runtime EOL/version updates are intentionally performed; do not implement an automatic dependency updater in application code.

## CI and publication

Create/reuse the smallest workflow that on release tags:

- checks tag equals Cargo/AppStream application version;
- runs standard Rust checks and the Flatpak smoke subset that CI's environment can support;
- builds from a clean checkout with pinned sources;
- runs `flatpak-builder-lint` for manifest/AppStream/repo;
- exports the OSTree repository and one single-file `.flatpak` bundle, signs them with the one project GPG key from CI secrets, and emits checksums;
- uploads the bundle as the release-candidate artifact. Pushing the exported repo to the permanent remote is a separate documented manual step performed only in WR-015.

Never commit private signing keys or print secrets. Record where the key lives and that losing it means users must re-add the remote; no rotation framework. One permanent remote — no staging remote, no protected promotion job, no SBOM tooling, and no Flathub submission (revisit Flathub only if real distribution demand appears, as a new ticket).

The application performs no update checking or self-replacement; Flatpak owns updates end to end.

## Install, update, rollback, uninstall

Document exact commands for:

- adding the permanent remote and installing stable;
- updating through Flatpak;
- installing a release-candidate `.flatpak` bundle over the installed app for testing;
- rolling back to the previous signed Flatpak commit;
- uninstalling while explicitly distinguishing app removal from optional user data deletion;
- migrating from the final AppImage and locating untouched legacy recordings/config.

Do not create a curl-pipe-shell installer or delete user data during uninstall/migration.

## Acceptance criteria

- A clean rebuild from the same commit uses identical locked inputs and produces an equivalent application payload.
- Fresh install from the candidate bundle, launch, update from a previous commit via a local copy of the repo, rollback, and uninstall preserve recordings/config as documented.
- Stable manifest contains no Devel IDs/probes, broad unproven permissions, unpinned downloads, debug files, or AppImage/updater code.
- Repository/bundle validates and signature/checksum verification succeeds using only documented public material.
- The permanent-remote publication step is documented, manual, secret-safe, and unexercised until WR-015.
- AppStream metadata and installation/migration instructions are accurate and English-only.

## Verification

Run standard checks, clean release Flatpak build, all three lints, payload/dependency/license inspection, one rebuild comparison, and install/update/rollback/uninstall against a disposable test user using the candidate bundle and a local copy of the repo. Record commands, outputs, artifact hashes/sizes, and approved exceptions. Do not publish to the permanent remote in this ticket.

## Not in scope

Flathub submission, maintaining AppImage/deb/rpm/snap, in-app downloading, automatic user-data deletion, staging or multiple custom remotes, or release automation unrelated to this application.
