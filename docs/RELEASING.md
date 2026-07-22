# Releasing

Flatpak is the only native release format. The stable application ID is
`io.github.JohanWes.WarcraftRecorder`; the permanent signed remote is
`https://johanwes.github.io/wow-recorder-linuxwayland/`.

## Candidate build

1. Update `native/Cargo.toml` and the first `<release version="...">` in
   `data/io.github.JohanWes.WarcraftRecorder.metainfo.xml` to the same version.
2. Run the four standard Rust checks from the root.
3. Create an annotated tag whose name is exactly `v<version>` and push it. The
   tag workflow rejects mismatches before building.
4. Configure the `release` environment secrets:
   `FLATPAK_GPG_PRIVATE_KEY` contains the armored private key and
   `FLATPAK_GPG_KEY_ID` contains only its public key ID. Never commit the key
   or print either secret.
5. Download the workflow's release-candidate artifact and verify the recorded
   SHA-256. The bundle is suitable for disposable-user testing and does not
   configure a remote.

CI builds from the locked `native/Cargo.lock` and `flatpak/cargo-sources.json`,
uses the pinned GNOME 50 SDK/runtime, runs `flatpak-builder-lint` for manifest,
AppStream, and repository, and exports a signed static OSTree repository plus
one `.flatpak` bundle. The documented linter exceptions in
`flatpak/lint-exceptions.json` are intentional: WR-000 fixes the canonical
reverse-DNS ID, the product is Wayland-only, and the read-only legacy config
grant is required for one-way migration. Rebuilding from the same commit should produce the same
application payload; OSTree/bundle container metadata may vary by build time.
The AppStream screenshot is served from the committed `main` tree, while the
candidate repository also carries its mirrored `screenshots/x86_64` ref.

## Manual remote publication (WR-015 only)

Do not run this step for WR-014 or for an unapproved candidate. After WR-015
approves the exact tested commit, publish the `repo/` directory produced by
that build to the project GitHub Pages site at the permanent URL. Keep
`index.flatpakrepo`, `summary`, the signed summary, objects, and static deltas
together. Verify with only the public key:

```sh
gpg --import public-release-key.asc
flatpak remote-add --user --if-not-exists warcraft-recorder \
  https://johanwes.github.io/wow-recorder-linuxwayland/index.flatpakrepo
flatpak install --user warcraft-recorder io.github.JohanWes.WarcraftRecorder
flatpak remote-info --user --show-commit warcraft-recorder \
  io.github.JohanWes.WarcraftRecorder
```

If the signing key is lost, users must remove and re-add the remote with a new
public key. There is no rotation framework or staging remote.

## Install, update, rollback, uninstall

For a stable install, add the permanent remote and install the application as
shown above. Update with `flatpak update --user`. For candidate testing use:

```sh
flatpak install --user ./warcraft-recorder.flatpak
```

To roll back to the previous signed deployment, inspect the remote log and
deploy its previous commit:

```sh
flatpak remote-info --user --log warcraft-recorder \
  io.github.JohanWes.WarcraftRecorder
flatpak update --user --commit=<previous-commit> \
  io.github.JohanWes.WarcraftRecorder
```

Uninstalling the app does not delete recordings or the untouched legacy
configuration:

```sh
flatpak uninstall --user io.github.JohanWes.WarcraftRecorder
```

Use `--delete-data` only when deleting the native app's private data is
intentional. The final AppImage migration imports
`~/.config/WarcraftRecorder/config-v3.json` once and leaves that file, the
recording directory, replay directory, and legacy sidecars untouched. A
rollback is simply launching the final AppImage again against those original
paths.
