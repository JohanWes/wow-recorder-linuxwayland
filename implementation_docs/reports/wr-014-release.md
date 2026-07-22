# WR-014 evidence: signed Flatpak release candidate

Status: **BLOCKED — candidate superseded; project signing material absent**

WR-015 production/manifest fixes supersede every candidate and hash recorded
below. The current unsigned rebuild is documented in `wr-015-final.md` and
passes the pinned SDK pipeline and smoke, but the repository has no `release`
environment, `FLATPAK_GPG_PRIVATE_KEY`, or `FLATPAK_GPG_KEY_ID`. The historical
disposable-key rehearsal below remains pipeline evidence, not a publishable
project-signed candidate.

This report covers the stable release manifest, metadata, tag workflow, local
candidate repository, signing rehearsal, and migration documentation. The
permanent remote was not published; WR-015 owns that step.

## Environment

- worktree parent: `8a63afa4` (`Document WR-013 release gate blocker`); the
  worktree contains the WR-014/WR-013 changes under review
- host: CachyOS, Linux 7.1.4-1-cachyos, KDE Plasma Wayland
- Flatpak: `1.18.0`
- flatpak-builder: `1.4.9` through `org.flatpak.Builder`
- Rust/Cargo: `1.93.1`
- AppStream: `1.1.3`
- stable ID: `io.github.JohanWes.WarcraftRecorder`
- runtime/SDK: GNOME `50`

## Contract checked

| Criterion | Evidence | Result |
|---|---|---|
| Stable manifest is pinned and release-only | `flatpak/io.github.JohanWes.WarcraftRecorder.yml`, `flatpak/cargo-sources.json` | PASS |
| Stable ID, command, metadata, icon, English copy, and screenshot | `data/io.github.JohanWes.WarcraftRecorder.{desktop,metainfo.xml}`, `data/icons/io.github.JohanWes.WarcraftRecorder.svg`, `data/screenshots/warcraft-recorder.png` | PASS |
| No development probes or debug payload | Stable manifest has no `development` feature or probe module; `/app/lib/debug` is removed by `cleanup-commands`; exported app ref has no debug paths; the workflow removes the separate Debug ref | PASS |
| Tag/version guard and standard CI checks | `.github/workflows/flatpak-release.yml`, `scripts/check-release-version.sh` | PASS; `v0.1.0` checked locally |
| Manifest/repository lint | Builder linter, with `flatpak/lint-exceptions.json` | PASS |
| AppStream lint | Release workflow runs it on `main`/tags after the committed screenshot is reachable | Deferred locally until this change is on `main`; see Known limitations |
| Candidate install, launch, update, rollback, uninstall | `scripts/flatpak-release-smoke.sh` and signed disposable repository | PASS |
| Signed repository, bundle, and checksums | Disposable GPG key rehearsal; no private key stored | PASS |
| Permanent remote is manual and not published | `docs/RELEASING.md`, `scripts/prepare-flatpakrepo.sh` | PASS; WR-015 gate preserved |

## Commands and raw results

```text
bash scripts/check-release-version.sh v0.1.0
release version: 0.1.0 (Cargo.toml and AppStream agree)

bash -n install.sh scripts/*.sh
git diff --check
PASS

cargo fmt --manifest-path native/Cargo.toml --check
PASS

cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings
Finished `dev` profile [unoptimized + debuginfo]

cargo test --manifest-path native/Cargo.toml --all-targets
76 library + 46 UI binary + 7 vertical-slice tests; 129 passed, 0 failed

cargo build --manifest-path native/Cargo.toml --release
Finished `release` profile [optimized]

flatpak run --command=flatpak-builder-lint org.flatpak.Builder \\
  --exceptions --user-exceptions flatpak/lint-exceptions.json \\
  manifest flatpak/io.github.JohanWes.WarcraftRecorder.yml
PASS

flatpak run --command=flatpak-builder-lint org.flatpak.Builder \\
  --exceptions --user-exceptions flatpak/lint-exceptions.json \\
  repo /tmp/wr014-release-final.1Nldt4/repo
PASS
```

The clean release build used the stable manifest and offline locked Cargo
sources. Its app commit was:

```text
fa84229568bf99296a4fbac99e40895894fb9388b24fc521881228c1ca841a89
```

The single-file candidate was 5,333,176 bytes:

```text
c7bda1b5bb27eca6f4a4df4e710a23d41fa8eeeee3852ca105b02f06e94a45eb  warcraft-recorder.flatpak
4a826cdd70262a5e586d2e22aac3095d4b766d434b33185e5536d35887bbc880  repo/summary
```

The exported application payload was 19 MiB including the bundled native
runtime libraries and had no `/app/lib/debug` paths. A second clean build had
commit `a88bf9bc30119235ce7623ec193c354e5e0896c2f13bbaedf661e19f7c6aa35c`;
the relative-file content hash matched the first build:

```text
b66abe3b2e32595163f76259e696a5547c286c76b741f205755100de1b6c5bc3
```

## Manual scenarios

| Scenario | Preconditions | Steps | Expected | Actual | Pass |
|---|---|---|---|---|---|
| Candidate install/launch/remove | Local unsigned candidate repo | Run `scripts/flatpak-release-smoke.sh repo io.github.JohanWes.WarcraftRecorder` | Stable ref installs, launches, and removes | Passed; permissions were Wayland, PulseAudio, DRI, read-only legacy config, and StatusNotifierWatcher | Yes |
| Signed install/launch/remove | Disposable GPG-signed repo and public key | Run the same smoke helper with `FLATPAK_GPG_PUBLIC_KEY` | Signature verification succeeds without private material | Passed; app commit `dd62d33ee30f...` | Yes |
| Update and rollback | Combined signed local repo contains two app commits | Install first commit, point its origin to the combined repo, update, then deploy the first commit | Both deployments work and rollback returns the original commit | Passed by the smoke helper | Yes |
| Bundle install/launch/remove | Signed `.flatpak` bundle | `flatpak install --user ./warcraft-recorder-signed.flatpak`; run with `--command=true`; uninstall | Bundle installs and launches without a remote | Passed; bundle SHA-256 `503cdfd0886065308fa1f54691b6faad0de6f537268f53acbfa72ab3b099da40` | Yes |
| Migration script without Flatpak | `flatpak` absent from a disposable `PATH` | Run `install.sh` | No privilege escalation; installation URL and manual commands are printed | Passed; exit status 1 with manual instructions | Yes |
| Migration script against signed remote | Disposable signed repo and descriptor override | Run `install.sh` with `WARCRAFTRECORDER_REMOTE_DESCRIPTOR=file://.../index.flatpakrepo` | Existing updater-compatible markers, user install, and launch instructions | Passed; app and remote were removed afterward | Yes |

## Files/artifacts

- `flatpak/io.github.JohanWes.WarcraftRecorder.yml` — stable manifest with
  immutable source pins, narrow Wayland permissions, release build, and
  license files.
- `flatpak/io.github.JohanWes.WarcraftRecorder.flatpakrepo.in` — safe remote
  descriptor template; `scripts/prepare-flatpakrepo.sh` injects only the
  exported public key and URL.
- `flatpak/lint-exceptions.json` — intentional local-remote exceptions for
  canonical app ID, Wayland-only operation, one-way config migration, and the
  pre-publication AppStream screenshot ordering.
- `data/screenshots/warcraft-recorder.png` — native application screenshot,
  1412x900 PNG, SHA-256
  `ff5931e5a919cc6f4a7589bd1c6656c8cec1946b37eded3b1f0ce2919c2fc412`.
- `.github/workflows/flatpak-release.yml` — pinned Rust/checkout/action
  inputs, PR/main candidate build, tag-signed candidate build, lints, smoke,
  and checksummed artifact upload.
- `scripts/check-release-version.sh` — tag/Cargo/AppStream consistency guard.
- `scripts/prepare-flatpakrepo.sh` — signed summary/static-delta/index
  generation; it never exports private key material.
- `scripts/flatpak-release-smoke.sh` — disposable install/launch/update/
  rollback/uninstall rehearsal.

## Decisions and deviations

- The stable ID remains the WR-000 maintainer-approved
  `io.github.JohanWes.WarcraftRecorder`, even though the GitHub repository slug
  is `wow-recorder-linuxwayland`; the URL-related linter exception records that
  deliberate mismatch.
- GNOME 50 remains pinned for this release. Updating the runtime/SDK requires
  a deliberate manifest and lock-source change followed by the same candidate
  checks; no automatic dependency updater was added.
- The project GPG private key is represented only by the CI secret
  `FLATPAK_GPG_PRIVATE_KEY`; its public ID is `FLATPAK_GPG_KEY_ID`. The local
  signature rehearsal used a disposable one-day key and was deleted from the
  repository/worktree.
- The remote URL and migration script remain documented but the permanent
  remote is not published. WR-015 must approve and publish the exact tested
  repository.

## Known limitations

- The source screenshot URL currently returns 404 because this worktree has
  not been merged to `main`. Direct AppStream lint therefore reports the
  expected `screenshot-image-not-found`; the workflow intentionally defers
  that one network-dependent check on pull requests and runs it on `main` and
  release tags. The committed screenshot and mirrored OSTree ref are present.
- The host does not have standalone `flatpak-builder` or `shellcheck`; the
  Flatpak Builder application supplied the build/lint tool, and shell syntax
  was checked with `bash -n`.
- The full parity and performance release gates remain WR-015 work.

## Approval

- reviewer/maintainer: pending WR-015 stable publication approval
- date/result: 2026-07-22 — release-candidate pipeline and signed local remote
  rehearsal complete; permanent remote intentionally unpublished
