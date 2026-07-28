# Releasing

Flatpak is the only native release format. The stable application ID is
`io.github.JohanWes.WarcraftRecorder`; the permanent signed remote is
`https://johanwes.github.io/wow-recorder-linuxwayland/`.

## Candidate build

1. Update `native/Cargo.toml` and the first `<release version="...">` in
   `data/io.github.JohanWes.WarcraftRecorder.metainfo.xml` to the same version.
2. Run `bash scripts/generate-release-notes.sh <version>` as the last thing
   before the release commit. It prepends the commit subjects since the
   previous tag to `data/release-notes.md`, which is compiled into the binary
   and shown once, after the update, by the "What's new" dialog. The release
   commit itself is not in the list; keep it a version-bump-only commit.
3. Run the four standard Rust checks from the root.
4. Create an annotated tag whose name is exactly `v<version>` and push it. The
   tag workflow rejects a version whose Cargo, AppStream, or release-notes
   entries disagree before building.
5. Configure the `release` environment secrets:
   `FLATPAK_GPG_PRIVATE_KEY` contains the armored private key and
   `FLATPAK_GPG_KEY_ID` contains only its public key ID. Never commit the key
   or print either secret.
6. Download the workflow's release-candidate artifact and verify the recorded
   SHA-256. The bundle is suitable for disposable-user testing and does not
   configure a remote.

CI builds from the locked `native/Cargo.lock` and `flatpak/cargo-sources.json`,
uses the pinned GNOME 50 SDK/runtime, runs `flatpak-builder-lint` for manifest,
AppStream, and repository, and exports a signed static OSTree repository plus
one `.flatpak` bundle. The documented linter exceptions in
`flatpak/lint-exceptions.json` are intentional: the canonical reverse-DNS ID,
the Wayland-only product scope, and the read-only legacy config grant required
for one-way migration. Rebuilding from the same commit should produce the same
application payload; OSTree/bundle container metadata may vary by build time.
The AppStream screenshot is served from the committed `main` tree, while the
candidate repository also carries its mirrored `screenshots/x86_64` ref.

## Manual remote publication

Never run this for an unapproved candidate. After a candidate is approved,
publish the `repo/` directory produced by that build to the project GitHub
Pages site at the permanent URL. Keep
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
recording directory, replay directory, and legacy sidecars untouched.

## Migrating the final AppImage users

The shipped AppImage (7.7.1) checks `releases/latest` on this repository,
compares the tag's version against its own, and pipes
`main/install.sh` into bash. That binary cannot be changed any more, so the
migration is three publications that have to be in place together:

1. `install.sh` on the **main** branch is the migration helper. The updater
   always fetches the branch tip, never a release asset.
2. The signed repository is published at the permanent URL, so
   `index.flatpakrepo` resolves.
3. A GitHub release is marked **Latest** whose tag parses as a version above
   `7.7.1`. The updater strips a leading `linux-`/`v` and a trailing commit
   hash, so the tag must look like `linux-7.7.2-<short-sha>`. A tag such as
   `v1.0.0` reads as *older* than 7.7.1 and the update button stays silent,
   and `linux-7.7.2-migration` parses as `7.7.2-migration` → `7.7.0`. That
   release carries no assets; it exists to trigger the migration and to hold
   the announcement.

Publish `v1.0.0` first (that tag drives the signed build), then create the
`linux-7.7.2-<short-sha>` release from the same main commit and let it be the
latest one. The native application never checks GitHub, so nothing regresses
by keeping a 7.x tag as the repository's latest release.

`install.sh` is also the installer the README hands to new users, so the
AppImage steps run only when an AppImage install is actually present. In that
case it installs the Flatpak, deletes the AppImage, replaces
`~/.local/bin/warcraftrecorder` with a shim that runs the Flatpak, deletes the
AppImage menu entry and icon, repoints any "run at start-up" entry, launches
the native app, and closes the running AppImage. Without `flatpak` on the host
it changes nothing, prints the distribution commands that install it, and
exits nonzero, which the updater shows as an error.

That last step sweeps by executable rather than signalling one process: on a
successful update the 7.7.1 updater relaunches itself *after* this script
returns, so a single signal loses the race and leaves two Warcraft Recorders
running. The sweep covers the deleted binary, the parked copy an earlier
revision of the script left behind, and the mounted AppImage payload.

A rollback is downloading the AppImage from the `linux-7.7.1-43e3ebf` release,
which still carries it and its checksum. It reads the untouched
`config-v3.json`, recordings, and sidecars.
