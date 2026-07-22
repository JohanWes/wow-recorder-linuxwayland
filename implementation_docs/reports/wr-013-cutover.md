# WR-013 evidence: Electron cutover gate audit

Status: **BLOCKED**

This report records the safe pre-cutover audit at commit `f9d92912`.
The destructive cutover was not started because the ticket's dependency and
publication gates are not satisfied.

## Work log: acceptance criteria copied before coding

- The final AppImage migration artifact/update notice was actually published and verified by an existing installation before deletion.
- Native import/restart/rollback scenarios preserve user recordings, tags, protection, config, and correlations.
- No shipped Electron/web stack, disabled product code, non-Linux code, localization framework, or AppImage builder remains.
- `native/` was not moved and no compatibility architecture was added.
- A release candidate from the exact post-cutover commit is available for WR-015; nothing is published to the permanent remote yet.
- Standard checks and full Flatpak smoke pass after deletion.
- Root docs, tree/LOC/dependency/search evidence, artifact hashes, and maintainer decisions are recorded.

## Environment

- audit commit: `f9d92912` (`Complete WR-012 settings, native choosers, manual/test controls, and status`)
- host: CachyOS, Linux 7.1.4-1-cachyos, KDE Plasma Wayland
- Rust: edition 2024 package under `native/`
- Flatpak: `/usr/bin/flatpak`; `flatpak-builder` is not installed
- legacy JavaScript dependencies: not installed (`node_modules/` absent)

## Contract checked

| Acceptance criterion / gate | Evidence | Result |
|---|---|---|
| WR-014 is complete before WR-013 | `implementation_docs/README.md` lists WR-014 as `TODO`; `implementation_docs/reports/wr-014-release.md` is absent | **BLOCKED** |
| Stable release candidate and permanent remote are available | Only `flatpak/io.github.JohanWes.WarcraftRecorder.Devel.yml` and matching `.Devel` metadata exist; stable manifest, stable metadata, and remote descriptor are absent | **BLOCKED** |
| Final AppImage migration artifact was published and verified | No final AppImage artifact, signature, existing-install verification log, or publication record is present in the checkout | **BLOCKED** |
| Phase B import/restart/rollback rehearsal | No release-candidate migration install is available; only synthetic native fixtures exist in `tests/native/fixtures/legacy/` | **NOT RUN** |
| Phase C deletion | Electron files, AppImage packaging, and the old updater remain intentionally | **NOT STARTED** |
| Native package remains in `native/` | `native/Cargo.toml` and `native/src/` are unchanged | **PASS** |

The missing WR-014 release candidate and the required publication/verification
authority are external state, not implementation details that can be safely
inferred. The ticket explicitly says not to begin Phase C in this condition.

## Commands and raw results

```text
cargo fmt --manifest-path native/Cargo.toml --check
PASS

cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings
PASS

cargo test --manifest-path native/Cargo.toml --all-targets
PASS — 76 library + 46 UI binary + 7 vertical-slice tests; 129 passed, 0 failed

cargo build --manifest-path native/Cargo.toml --release
PASS

npm test
NOT RUNNABLE — node_modules/ is absent; npm reports `jest: command not found`.
The legacy suite is a Phase C deletion target and was not restored merely to
cross a gate that cannot authorize deletion.

git status --porcelain=v1
CLEAN before this report was created
```

The first sandboxed Clippy attempt failed because Cargo could not write
`native/target`; the same command was rerun with the approved build-cache
permission and passed. The release build was verified the same way.

## Lean audit (pre-cutover)

The tracked tree currently contains the following top-level counts:

```text
.erb 26       .github 3       .vscode 4       assets 130
data 17       docs 9          flatpak 4       implementation_docs 34
native 33     release 4       root 13         scripts 2
src 154       tests 78
```

The raw `wc -l` count for `native/src/**/*.rs` is 24,569 lines. This includes
inline test modules and comments, so it is an upper bound rather than a
production-only LOC measurement; no LOC counter is installed on this host.
The direct dependency count from `native/Cargo.toml` is 11, below the README
target of 14 and hard-review threshold of 16. The raw LOC figure is above the
12,000 target and 18,000 hard-review threshold and requires maintainer review
before WR-013 can be marked done.

Before deletion, the term search reports these file counts:

```text
electron 77   node 63       react 103      webpack 23
tailwind 12   storybook 1   ipc 36         cloud 43
upload 22     download 28   chat 24        obs 87
windows 33    macos 5       localization 13  localisation 53
appimage 18
```

These hits are expected in the legacy source, packaging, tests, and historical
implementation reports at this stage. A post-deletion search must be rerun and
classified after WR-014 supplies the stable release metadata. No assets or
native modules were deleted during this audit, so an unused-payload audit
cannot truthfully be reported as complete.

## Manual scenarios

| Scenario | Preconditions | Steps | Expected | Actual | Pass |
|---|---|---|---|---|---|
| Final AppImage migration | Published final AppImage and WR-014 remote | Run the shipped updater against the release candidate | User-level Flatpak install, native import, and launch offer | No final artifact or permanent remote is available | No |
| Native rollback | Native migration rehearsal completed | Start the final AppImage after native use | Original recordings/config remain available | Not run | No |
| Post-deletion Flatpak smoke | Exact post-deletion release candidate | Exercise the WR-014/WR-015 smoke scenario | Fresh setup, capture, library, playback, media jobs, and shutdown pass | No post-deletion candidate exists | No |

## Decisions and deviations

- **Do not delete Phase C files yet.** WR-013 requires WR-014 `DONE` and an
  actually published/verified final AppImage migration release first.
- **Do not invent a remote URL or stable package identity.** The current
  checkout contains only the development Flatpak manifest; WR-014 owns the
  stable manifest and permanent remote details.
- **Do not claim the native LOC gate passes.** The raw source count exceeds the
  documented target and needs a maintainer-approved deletion/design review.

## Known limitations

- WR-013 is not complete. It is blocked on WR-014's release candidate and on
  the required release publication/verification authority.
- The root README, updater script, old CI workflow, and legacy tree remain so
  the final AppImage can still be built and migrated once the release gate is
  authorized.
- Full Flatpak smoke, migration rehearsal with anonymized real user copies,
  release artifact hashing/signing, and existing JavaScript tests remain
  pending the release-candidate environment.

## Approval

- reviewer/maintainer: pending release authority and WR-014 completion
- date/result: 2026-07-22 — blocked; no Phase C deletion performed
