# WR-015 evidence: final parity, lean, and publication gates

Status: **BLOCKED — local engineering gates pass; publication prerequisites remain**

## Work log: acceptance criteria copied before coding

- Every `KEEP` feature passes with cited evidence; only localization and approved
  `REMOVE_*` items are absent.
- All README hard size/memory/start/filter/stall and code/dependency gates pass,
  or have an explicit maintainer exception where the contract permits one.
- Long-running capture/playback has no diagnosed leak or runaway process, and
  media jobs never freeze the GTK thread.
- Sandbox denial, process cleanup, signing/update/rollback, migration, and
  license checks pass.
- UI brief and accessibility review are signed by a maintainer.
- This report lists exact commands, raw samples/medians, artifact hashes,
  screenshots/traces, failures/fixes, skipped speculative work, and remaining
  known limitations.

No stable publication is authorized by this status line. Publication remains
the last action and may occur only after every gate below is evidenced against
one unchanged signed artifact.

## Outcome

The implementation and reproducible local gates are ready. This ticket is not
`DONE` and nothing was published: WR-013's final AppImage migration release is
absent; the GitHub repository has no `release` environment or project GPG key;
the required 60-minute armed and 30-minute interactive playback sessions,
denial matrix, light/dark/narrow/200% accessibility review, and maintainer
sign-off remain unperformed. These explicit gates cannot be waived by an
implementation report.

The recorded Flatpak candidate is unsigned and was built from the pre-final-
ization working-tree parent `8a63afa441550ccd8e451ad5308c80daf1e3dc40`;
binary diff SHA-256 is
`dfc3443aaf7ea928f83285385bcc7b06358e4390ee23e7ec2b46673063fb7df1`.
Those artifact measurements are retained as historical evidence and do not
prove the later committed tree. Against the final committed tree, host
verification was rerun on 2026-07-22:

- `cargo fmt --manifest-path native/Cargo.toml --check`: PASS
- `cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings`: PASS
- `cargo test --manifest-path native/Cargo.toml --all-targets`: 79 library,
  47 UI, and 7 vertical-slice tests passed; 2 release-gate tests remained
  intentionally ignored
- `cargo build --manifest-path native/Cargo.toml --release`: PASS

The Flatpak artifact and every remaining release gate must be rebuilt and
rerun from the final committed tree before release.

## Environment and deterministic corpus

- CachyOS, Linux 7.1.4-1-cachyos, KDE Plasma Wayland
- AMD Ryzen 7 9800X3D, 30 GiB RAM, Kingston NVMe
- Flatpak 1.18.0; Flatpak Builder 1.4.9; GNOME Platform/SDK 50
- bundled GSR 5.13.9, FFmpeg 8.0.1, Clapper 0.10.0
- `tests/native/generate_performance_corpus.py` SHA-256:
  `a4cee436895c6cc0d264d2d512dc5e6c381865ae3199022bbc193eef5e437511`
- generated corpus: 2,000 sidecars + 2,000 zero-byte placeholders, 1,900
  correlation groups; manifest SHA-256:
  `c6c46b8236155d7b476785019ca7044a389b186d5393ec77814f5e1913a73801`

## Gate results

### Parity

The authoritative row-by-row classification remains
`reports/wr-000-feature-parity.md`. All retained non-publication behavior has
native evidence across the activity/parser goldens, 78 library tests, 46 UI
tests, seven vertical-slice tests, and WR-002/006/007 real sandbox capture,
H.264/AV1 playback, clip, montage, tray, and process proofs. This covers setup
and retained settings; every flavour/category; activity completion, loss,
abandon, discard, force-end and restart; manual/test capture; table families,
columns, chips, dates and actions; storage protection/limits; transport,
shortcuts, timeline, drawing and clips; single-view POV selection and montage;
status, logs, About, tray and no-watcher fallback.

The final AppImage migration/Flatpak update row is **BLOCKED**. Approved
removals are unchanged: unreachable manual hotkey/autostart, synchronized POV
grid (selector/montage retained), native updater UI, localization, disabled
cloud/account/chat/pro, non-Linux/OBS packaging, and post-migration AppImage.

### Lean audit

- `/app` payload: 19,464,625 bytes (18.56 MiB), below 100 MiB
- 11 direct normal Cargo dependencies, below target 14/hard limit 16
- production Rust/UI: 17,740 nonblank lines after excluding test modules
  (19,075 raw including comments/blanks), below the 18,000 production-code
  review threshold under this recorded measure
- bundled executables: only app, GSR and FFmpeg; minimal FFmpeg/x264 and
  Clapper libraries/plugins support retained media rows
- all ELF payloads stripped; no `/app/lib/debug`; headers, pkg-config, docs and
  locales removed
- forbidden-term audit found no Electron runtime/build source; remaining hits
  are historical/migration evidence, dependency names, or Rust `.windows(2)`
- a release audit found the legacy GPLv2 root text mislabeled as the native
  GPLv3 license. Both manifests now install actual GPLv3 text as
  `WarcraftRecorder-GPL-3.0-or-later` (SHA-256
  `3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986`).

### Performance

One warm-up plus five exact-Flatpak measured runs:

| Metric | Warm-up | Samples | Median / budget |
|---|---:|---|---|
| Cold post-scan readiness (ms) | 102 | 98, 87, 98, 88, 89 | 89 / <1,000 PASS |
| Idle RSS (KiB) | 148,008 | 152,764, 151,696, 151,196, 149,176, 149,880 | 151,196 (147.65 MiB) / <153,600 PASS |
| Sidecar scan (µs) | discarded | 17,651, 17,720, 17,948, 18,082, 18,496 | 17,948 PASS |
| Suggestion (µs) | discarded | 1,451, 1,451, 1,452, 1,505, 1,709 | 1,452 / <50,000 PASS |
| Chip filter (µs) | discarded | 3,716, 3,970, 8,186, 8,189, 8,462 | 8,186 / <50,000 PASS |
| Date filter (µs) | discarded | 164, 166, 169, 170, 172 | 169 / <50,000 PASS |
| Sort (µs) | discarded | 477, 484, 485, 486, 490 | 485 / <50,000 PASS |

Readiness ends at the coordinator's post-scan log result; RSS is read after two
idle seconds. The UI harness drives the real SearchEntry, suggestion popover,
GtkFilterListModel and GtkSortListModel, forcing lazy result enumeration. The
app applies the 2,000-entry snapshot and materializes its actual selected
200-row category. Immutable library vectors are shared with `Arc`.
`MALLOC_ARENA_MAX=2` is scoped to the GTK process; GSR/FFmpeg commands remove it.

The 60/30-minute sessions and GTK frame trace remain **NOT RUN**.

### Sandbox, safety, UI and accessibility

- permissions remain Wayland, PulseAudio, DRI, StatusNotifierWatcher, and
  read-only legacy config; manifest/repository lint passed
- update/delete accepts only direct children of the flat storage root and
  rejects leaf/directory symlink escapes; regression tests pass
- zero-byte corpus files scan as unusable media; the player shows a recovery
  state and disables every transport/seek/frame/draw/clip action
- child cancellation/failure/shutdown tests pass; earlier real tray Quit proof
  reaped GSR; GSR/FFmpeg do not inherit the GTK allocator setting
- populated AppStream screenshot: `data/screenshots/warcraft-recorder.png`,
  2086×1330 physical for 1440×900 logical, SHA-256
  `c0278ada320b4605b83b44c811df8ad1357ab56518f434ae4bc699cddf991eb3`
- icon-only player/drawing/montage/settings/Creator controls have accessible
  labels; bulk selection announces its exact count

The final denial/manual accessibility matrix and maintainer UI sign-off remain
pending. Signed update/rollback passed only in WR-014's disposable-key rehearsal.

## Final local artifact and verification

The OSTree, bundle, and repository hashes below belong to the prior unsigned
candidate described above. They are not hashes of the final committed tree
and must not be published.

- OSTree app commit:
  `9815018b868bfe74aad3344058082c05dfc29c313e8f26ca6aef17acefc39a38`
- bundle SHA-256:
  `a3774b55c0b7c05fd8d8991f53a7af7fef8f0397f1c27245b400f67ff4a3b541`
- repo summary SHA-256:
  `3a37fab4b68f9e8bdacfe0b9bbb4544687053088a084aae4ac89271b171d6ec1`
- prior candidate host fmt, Clippy `-D warnings`, all 131 normal tests and
  release build: PASS
- current commit host fmt, Clippy `-D warnings`, all 133 non-ignored tests and
  release build: PASS (details recorded above)
- pinned GNOME 50 sandbox fmt/Clippy/tests/release build: PASS
- manifest/repository lint, shell syntax, version guard, and unsigned fresh
  install/launch/remove: PASS

Earlier candidates/hashes in WR-014 are superseded. Failures fixed during this
gate were zero-byte corpus rejection, snapshot duplication/RSS, helper-only UI
benchmarking, nested-path symlink exposure, active controls on empty media,
unpinned Ubuntu CI native checks, an empty/error AppStream screenshot, missing
accessible labels/count text, and the mislabeled GPL text. Each production or
manifest fix caused a full rebuild; this is the last local rebuild.

No cache, async runtime, database, thumbnailer, worker pool, web layer, custom
allocator, or second player architecture was added.

## Publication disposition

- independent review: requested; findings above fixed; follow-up pending
- maintainer UI/accessibility sign-off: **pending**
- project signing key/release environment: **absent**
- WR-013 published AppImage migration: **pending**
- stable publication: **NOT PERFORMED**

Next: rebuild/sign the final committed tree through WR-014, run the outstanding
duration/UI/denial/migration checks against that signed artifact, obtain
maintainer approval, then publish it unchanged.
