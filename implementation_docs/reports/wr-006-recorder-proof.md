# WR-006: real GSR recorder proof

## Environment

- Commit under test: b5879b76be5812e9377048fa97a236ff6c979b80 plus the WR-006 fixes in this worktree.
- Linux 7.1.2-3-cachyos, KDE Plasma, native Wayland (wayland-0), x86_64.
- Flatpak 1.18.0; GNOME Platform/SDK 50; manual-proof Devel app commit b6e372d73783a03deca39c0b16fe23c0fbe06953e351db057bb0a6021483ab9a; final verification rebuild commit 54336eb31ca9ebf85ae1796d56fcdd115512c5376db4ff20c116b5f82560363d.
- Bundled gpu-screen-recorder 5.13.9; flatpak-builder 1.4.10.

## Contract checked

| Acceptance criterion | Evidence |
|---|---|
| Exact GSR arm, replay/regular save, audio, and shutdown inside Flatpak | Successful real capture below; both outputs contain H.264 video and AAC audio; GSR exited 0 and no GSR process remained. |
| Replay/regular TSV correlation and GSR-produced paths | gsr-events.tsv and reselect-events.tsv below contain timestamped replay and regular records with sandbox paths. |
| Target reselection | A fresh token path logged no restore token found followed by saved restore token; a new 22-byte token and a second replay/regular pair were produced. Fake-GSR denial/restore coverage remains green. |
| Audio discovery | Real Flatpak GSR output returned default output/input and live ALSA monitor/input devices. The parser now accepts the observed id|label form as well as the legacy whitespace form. |
| Automated lifecycle and process checks | SDK fmt, clippy, test, and release build passed; 57 library tests plus 1 tray test passed. |

## Commands and raw results

The real GSR process ran inside the app sandbox with the restored portal token and these capture arguments:

~~~text
gpu-screen-recorder -w portal -restore-portal-session yes \
  -portal-session-token-filepath /var/data/wr006-proof/portal-token \
  -r 8 -replay-storage ram -restart-replay-on-save no \
  -c mkv -f 30 -bm cbr -q 5000 -k h264 -ac aac -cursor no \
  -a default_output -o /var/data/wr006-proof/replay \
  -ro /var/data/wr006-proof/regular \
  -sc /var/data/wr006-proof/gsr-hook.sh -v no
~~~

Control sequence: wait 12 seconds for the first keyframe, send SIGUSR1, send numeric signal 34 (SIGRTMIN) to start regular recording, send it again to stop, then send SIGINT. Result: gsr-exit=0.

Real audio discovery inside the same Flatpak:

~~~text
default_output|Default output
default_input|Default input
alsa_output.usb-Logitech_G522_LIGHTSPEED_-_Wireless_Mode_0000000000000000-00.analog-stereo.monitor|Monitor of G522 LIGHTSPEED - Wireless Mode Analog Stereo
alsa_input.usb-Logitech_G522_LIGHTSPEED_-_Wireless_Mode_0000000000000000-00.mono-fallback|G522 LIGHTSPEED - Wireless Mode Mono
alsa_output.pci-0000_03_00.1.hdmi-stereo-extra2.monitor|Monitor of Navi 48 HDMI/DP Audio Controller Digital Stereo (HDMI 3)
~~~

The real capture event file was:

~~~text
1784532607171	replay	/var/data/wr006-proof/replay/Replay_2026-07-20_09-30-07.mkv
1784532617138	regular	/var/data/wr006-proof/regular/Video_2026-07-20_09-30-11.mkv
~~~

The reselect run used a new token path, produced a new token, and emitted:

~~~text
1784532683219	replay	/var/data/wr006-proof/reselect-replay/Replay_2026-07-20_09-31-23.mkv
1784532692186	regular	/var/data/wr006-proof/reselect-regular/Video_2026-07-20_09-31-27.mkv
~~~

Rust verification from the GNOME SDK environment:

~~~text
cargo fmt --manifest-path native/Cargo.toml --check       PASS
cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings  PASS
cargo test --manifest-path native/Cargo.toml --all-targets  57 passed, 1 passed
cargo build --manifest-path native/Cargo.toml --release    PASS
flatpak-builder --force-clean --user --install wr006-build flatpak/io.github.JohanWes.WarcraftRecorder.Devel.yml  PASS
~~~

## Manual scenarios

| Scenario | Preconditions | Steps | Expected | Actual | Pass |
|---|---|---|---|---|---|
| Arm and restore token | Existing portal token, disposable app-private output | Start bundled GSR with restore-portal-session yes | Buffer remains live | GSR negotiated the portal stream and accepted the restored token | Yes |
| Replay plus regular save | Armed buffer with audio | SIGUSR1, SIGRTMIN, SIGRTMIN | One replay and one regular hook event | Both events and artifacts returned; H.264/AAC probes passed | Yes |
| Audio discovery | Bundled GSR in the sandbox | gpu-screen-recorder --list-audio-devices | Stable IDs and labels, defaults present | Defaults and three live ALSA devices returned; parser tests pass for pipe and whitespace forms | Yes |
| Reselect success | Fresh token path | Start GSR with no token, wait for token, capture, shut down | New reusable token and capture target | New 22-byte token written; second replay/regular pair produced | Yes |
| Reselect denial/restore | Fake-GSR exit 60 fixture | Delete token, fail selection, restore prior token | Typed denial; prior token preserved | SelectionDenied and token restoration test passed | Yes |
| Shutdown | Live GSR child | Send SIGINT, wait/reap | Exit with no child | Exit 0; pgrep found no GSR process | Yes |

## Files/artifacts

- /home/johanw/.var/app/io.github.JohanWes.WarcraftRecorder.Devel/data/wr006-proof/replay/Replay_2026-07-20_09-30-07.mkv — 6,179,297 bytes, SHA-256 31f38b20ba81d59fc71572c4938ce09beb7cd42f7d6ccfaaeb3be62c63e9feac.
- /home/johanw/.var/app/io.github.JohanWes.WarcraftRecorder.Devel/data/wr006-proof/regular/Video_2026-07-20_09-30-11.mkv — 3,716,279 bytes, SHA-256 14bf9c1a52b0d713c00d9d4820a575b75feea9f1218bbb5549b119b3b9902939.
- /home/johanw/.var/app/io.github.JohanWes.WarcraftRecorder.Devel/data/wr006-proof/reselect-replay/Replay_2026-07-20_09-31-23.mkv — 5,437,115 bytes, SHA-256 c6e031aa95d273122a38f115a53c0dfc682cbfd986446892980001e7bf33691b.
- /home/johanw/.var/app/io.github.JohanWes.WarcraftRecorder.Devel/data/wr006-proof/reselect-regular/Video_2026-07-20_09-31-27.mkv — 3,090,085 bytes, SHA-256 e735b27fead3583bf896c4ca19b9c1ae9ceee4e9e137b1dc61625e4f9279097c.
- gsr-events.tsv SHA-256 88970d7cfc4a5e35c9485a8e8888802eec28ba0d87e54e48181a5fbc2bc5ef43; reselect-events.tsv SHA-256 afef577f0f6b064210ecd4fbeedc9b89cd673d37e32b046aebb36a14af738060.
- portal-token SHA-256 490c85dc7382909544053bb42adac10b4f7cff7da9bb0ec5b43b7b033608f03c; reselect-token SHA-256 b225334dadb01416446766ba569376c68caea851be1eac1379ebe85379130d79.

## Decisions and deviations

- The observed GSR 5.13.9 audio output uses id|label; WR-006 now accepts that real form and retains the source-traced whitespace form for existing fixtures.
- Reselection arms with the token omitted from the temporary config, so deleting the token actually reaches the portal. A successful selection is reported by poll; a denied selection restores the prior token.
- The first manual replay signal was sent before the first keyframe and GSR correctly rejected that replay while regular recording still worked. Repeating after a 12-second stabilization window passed; no adapter timing contract was changed.

## Skipped (YAGNI)

- No persistent restart journal or crash recovery was added; WR-000 explicitly assigns interrupted-capture cleanup to WR-007.
- No live denial dialog was forced because the desktop portal reused its remembered authorization; the denial/restore behavior is covered by the fake-GSR test and the real denial proof in WR-002.

## Known limitations

- These artifacts are disposable manual evidence under the user Flatpak data directory, not committed media fixtures. The actual GSR process is not wired into the coordinator until WR-008, as required by the ticket sequence.

## Approval

- Automated verification: 2026-07-20 — PASS.
- Manual proof: 2026-07-20 — PASS under the maintainer-authorized desktop session.
