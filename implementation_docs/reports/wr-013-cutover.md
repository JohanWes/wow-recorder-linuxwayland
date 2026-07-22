# WR-013 evidence: Electron cutover and migration gate

Status: **BLOCKED**

WR-015 re-audited this blocker on 2026-07-22. Public `main` still contains the
legacy Electron application and the latest public AppImage remains
`linux-7.7.1-43e3ebf`; there is no final migration update/notice or
existing-install verification. WR-014's artifact hashes below are superseded
by the unsigned local candidate in `wr-015-final.md`. Neither fact satisfies
this ticket's required publication gate.

The native/Flatpak cutover itself is present in the worktree, but the ticket
cannot be marked `DONE`: its contract requires an actually published final
AppImage migration release and verification by an existing installation
before completion. No release authority or publication was performed in this
turn. The exact post-deletion native candidate is ready for WR-015.

## Work log: acceptance criteria copied before coding

- The final AppImage migration artifact/update notice was actually published and verified by an existing installation before deletion.
- Native import/restart/rollback scenarios preserve user recordings, tags, protection, config, and correlations.
- No shipped Electron/web stack, disabled product code, non-Linux code, localization framework, or AppImage builder remains.
- `native/` was not moved and no compatibility architecture was added.
- A release candidate from the exact post-cutover commit is available for WR-015; nothing is published to the permanent remote yet.
- Standard checks and full Flatpak smoke pass after deletion.
- Root docs, tree/LOC/dependency/search evidence, artifact hashes, and maintainer decisions are recorded.

## Environment

- parent commit: `8a63afa4`
- host: CachyOS, Linux 7.1.4-1-cachyos, KDE Plasma Wayland
- Flatpak: `1.18.0`; Builder application: `1.4.9`
- Rust/Cargo: `1.93.1`
- legacy `node_modules/`: absent

## Contract checked

| Acceptance criterion / gate | Evidence | Result |
|---|---|---|
| Final AppImage migration release actually published and verified by an existing installation | No published artifact, signature, existing-install log, or release authority record is in the worktree | **BLOCKED** |
| Native migration script is user-level and failure-safe | `install.sh`; no-Flatpak fallback and signed disposable remote rehearsal are recorded in `reports/wr-014-release.md` | PASS locally; not verified through a real released AppImage |
| Native import/restart/rollback preserves user data | `native/src/config.rs`, `native/src/storage.rs`, legacy fixtures/goldens, native tests | Synthetic fixture coverage passes; real anonymized-data rehearsal not available |
| No shipped Electron/web stack or AppImage builder remains | Phase C deletion list below; post-cutover search below | PASS in the worktree, subject to historical-contract references |
| `native/` stays in place; no compatibility architecture added | `native/Cargo.toml`, `native/src/`, stable manifest | PASS |
| Exact post-deletion release candidate is available and unpublished | `reports/wr-014-release.md`; local stable candidate bundle/repository | PASS |
| Standard checks and post-deletion Flatpak smoke | Cargo checks plus `scripts/flatpak-release-smoke.sh` | PASS for the WR-014 smoke subset; full populated-library scenario remains WR-015 |
| Evidence, hashes, lean audit, and decisions recorded | This report, WR-014 report, root docs, workflow, scripts | PASS except the external publication evidence explicitly required above |

## Phase A and B evidence

The migration script adds the stable remote and installs the stable app only
with `flatpak --user`; it never invokes `sudo`, deletes user data, or replaces
the legacy config. If Flatpak is absent or a remote/install step fails it
prints the installation page and manual commands. It keeps the four legacy
updater progress marker shapes for the final AppImage parser.

The local rehearsal against a disposable signed repository passed the marker,
user-install, signature-verification, and launch paths. Native config and
sidecar migration tests pass against the anonymized fixtures in
`tests/native/fixtures/legacy/`, including byte-preserving legacy config
import and compatible sidecar patching. A real existing AppImage installation,
real anonymized user directory, and published final AppImage were not
available, so this report does not claim the external migration gate.

## Phase C deletion

At the user's explicit request, the legacy tree was removed after the WR-014
candidate was built, even though the ticket's original ordering says the
publication gate precedes deletion. This is a recorded process deviation, not
evidence that the publication criterion passed.

Deleted legacy surfaces include:

- `.erb/`, `.vscode/`, `src/`, `assets/`, and the old JS/TS test tree;
- `release/`, `package.json`, `package-lock.json`, TypeScript/Webpack/Tailwind
  configuration, and the old Node CI workflow;
- Electron/localization/platform/cloud/OBS source and the old update service;
- obsolete design/settings/localization/OBS documentation and helper scripts;
- old log fixtures and the old Python integration harness.

Retained native assets are under `data/`, fixtures/goldens under
`tests/native/`, and the development Flatpak remains separate under the
`.Devel` ID. The root docs now describe native Rust/GTK and Flatpak as the
only normal development/release path. Historical implementation contracts,
the migration-specific config/sidecar fixtures, the final AppImage migration
notice, and the combined license notices remain as evidence or required
transition material.

## Commands and raw results

```text
bash -n install.sh scripts/*.sh
git diff --check
PASS

cargo fmt --manifest-path native/Cargo.toml --check
PASS

cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings
PASS

cargo test --manifest-path native/Cargo.toml --all-targets
76 library + 46 UI binary + 7 vertical-slice tests; 129 passed, 0 failed

cargo build --manifest-path native/Cargo.toml --release
PASS

bash scripts/flatpak-release-smoke.sh /tmp/wr014-release-final.1Nldt4/repo io.github.JohanWes.WarcraftRecorder
Flatpak install, launch, candidate smoke, and app removal passed
```

The post-deletion stable candidate bundle SHA-256 is
`c7bda1b5bb27eca6f4a4df4e710a23d41fa8eeeee3852ca105b02f06e94a45eb`.
Its app commit is
`fa84229568bf99296a4fbac99e40895894fb9388b24fc521881228c1ca841a89`.
The exact repeated build produced a different OSTree commit because commit
metadata is time-dependent, but its relative-file payload hash matched:
`b66abe3b2e32595163f76259e696a5547c286c76b741f205755100de1b6c5bc3`.

## Lean audit

Current tracked top-level purpose counts are:

```text
.github 3       CHANGELOG/root docs 5       data 17
docs 7          flatpak 4                   implementation_docs 35
native 33       tests 21                    root/license/scripts 6
```

`native/src/**/*.rs` is 24,569 raw lines including inline tests and comments;
the direct dependency count is 11. The raw count is above the README's
18,000-line hard review threshold, so a production-only LOC/design review
remains a maintainer gate for WR-015; no speculative abstraction was added in
this cutover.

The post-cutover term search contains only legitimate categories: historical
CHANGELOG entries, implementation contracts/reports, the one-way legacy
config/sidecar fixtures, the final AppImage migration instructions, and
third-party license notices. No matching legacy source/package/build file
remains. The remaining `Electron` mention in the Devel manifest documents the
known read-only migration input and is required by the native import contract.

## Manual scenarios

| Scenario | Expected | Actual | Pass |
|---|---|---|---|
| Stable candidate fresh install/launch/remove | App installs from the exact post-deletion candidate and leaves user data | Passed in the WR-014 smoke helper | Yes |
| Signed update/rollback | A later signed commit updates, then the original commit redeploys | Passed against disposable combined OSTree repo | Yes |
| Legacy migration | Existing AppImage updater installs native app and native import starts | Local script/fixture rehearsal passed; no published AppImage or existing installation | **No — external gate** |
| Rollback to final AppImage | Original AppImage still sees untouched config, recordings, sidecars | Not run without a published final AppImage and real installation | **No — external gate** |
| Full cutover smoke | Populated real media/log data exercises every WR-000 retained path | WR-014 smoke subset and native fixture tests pass; full scenario belongs to WR-015 | **Not run** |

## Decisions and deviations

- WR-014 is now marked `DONE` in the dependency table. Its permanent remote
  remains unpublished as required by WR-015.
- Phase C deletion was performed at the user's explicit request after the
  exact post-deletion release candidate was built. The ticket's own Phase-A
  publication-before-deletion rule is therefore recorded as a deviation and
  remains unsatisfied.
- Do not mark WR-013 `DONE` or flip the migration script live until WR-015
  records release authority, final AppImage publication, existing-install
  verification, and the real migration/rollback rehearsal.

## Known limitations

- WR-013 remains blocked on the actual final AppImage/update-notice
  publication and existing-install verification. This cannot be inferred
  from a local candidate or a disposable GPG repository.
- Real anonymized user media/config and a populated running WoW installation
  were not available for the Phase-B/full-smoke scenarios.
- The raw native LOC count requires the maintainer-approved production-only
  lean review before the overall release gate can close.

## Approval

- reviewer/maintainer: pending actual AppImage release authority and WR-015
- date/result: 2026-07-22 — cutover files removed and candidate ready; ticket
  remains **BLOCKED** because its external publication criterion is unmet
