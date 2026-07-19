# WR-002: development Flatpak and platform proofs

## Environment

- Commit under test: `52abf2dd5333923f514c6a1da82494c00e82ef08` plus the WR-002 working tree on `refactor/native-non-frontend`.
- Test date/session: 2026-07-19, CachyOS, Linux `7.1.2-3-cachyos` x86_64, KDE Plasma Wayland (`WAYLAND_DISPLAY=wayland-0`).
- GPU: AMD `/dev/dri/card1`; GSR exposed H.264, HEVC, and AV1 hardware paths.
- Build tools: Flatpak `1.18.0`; standalone CachyOS `flatpak-builder` package `1.4.10-1.1` (download/extraction command and package hash below); and `org.flatpak.Builder` commit `d04b579250dbd3d6c53b5496824b6964022c35861d0e9959d0baf697356f0827` only for `flatpak-builder-lint` and `flatpak-cargo-generator`.
- Runtime/SDK: `org.gnome.Platform//50`, `org.gnome.Sdk//50`, and `org.freedesktop.Sdk.Extension.rust-stable//25.08` (`rustc 1.97.1`). The SDK supplies GTK `4.22.4`, libadwaita `1.9.2`, and GStreamer `1.26.11`.
- Runtime support: GNOME 50 was released on 2026-03-18. GNOME's published policy supports a runtime through the next-next stable branch's point-one grace period, so branch 50 is supported through GNOME 52.1, expected April 2027. No exact GNOME 52.1/EOL calendar date had been published by the test date; inventing one would be inaccurate.

## Contract checked

| Acceptance criterion | Evidence/result |
|---|---|
| Clean Flatpak build, Cargo offline | **PASS.** The explicitly downloaded/extracted standalone `flatpak-builder` ran `--force-clean --user --install` successfully. The application module ran `cargo --offline build`; Cargo sources came from `flatpak/cargo-sources.json`. |
| Installed Devel app, Wayland, metadata | **PASS.** `io.github.JohanWes.WarcraftRecorder.Devel/x86_64/master` launched natively on Wayland. Desktop validation, pedantic AppStream validation, and builder AppStream composition passed. |
| Narrow manifest permissions | **PASS.** Only Wayland, PulseAudio, DRI, one SNI bus name, and one exact read-only legacy directory are static grants. No network, X11, blanket home/host, PipeWire filesystem, or bus wildcard exists. |
| Persistent folder authorization | **PASS for the WR-002 platform gate.** Three real `GtkFileDialog` selections outside app-private data survived restart. Rust, GSR, and FFmpeg accessed every persisted document path; an unselected absolute host path remained denied. Config authorization-state and scan/eviction guards do not exist yet and remain explicitly owned by WR-003/WR-007. |
| Real sandboxed capture | **PASS.** Portal selection, restored token, RAM replay buffer, replay save, regular start/stop, AAC audio, H.264, AV1, cancel, reselection, and graceful shutdown were exercised. |
| Real H.264 and AV1 playback | **PASS.** The retained ClapperGtk widget played the GSR-produced H.264/AAC and AV1/AAC recordings and passed the complete control matrix below. |
| FFmpeg clip and montage | **PASS.** The shipped minimal executable produced a stream-copy clip and a two-source H.264+AV1 transitioned/audio montage. Both reopened successfully inside the sandbox. |
| Tray/background lifecycle | **PASS.** A live capture survived hide, tray Activate reopened the app, and tray Quit gracefully stopped/reaped GSR before the tray service shut down. A separate isolated D-Bus session retained the manifest permission but contained no watcher; close exited instead of hiding. |
| License compatibility/notices | **PASS.** Native code is GPL-3.0-or-later per ADR-001. All bundled native components are compatible and their applicable notices are installed under `/app/share/licenses/$FLATPAK_ID`. |

WR-000 did not contain committed legacy media samples. With maintainer approval, the real sandboxed GSR outputs created by this ticket were used for the playback and transform gates. They exercise the exact required H.264, AV1, and AAC paths but are not claimed to have legacy provenance.

## Manifest permissions

| `finish-args` line | Why it exists | Observed behavior |
|---|---|---|
| `--socket=wayland` | GTK/Clapper rendering and portal-based capture | Installed app and embedded player launched on Wayland. GSR negotiated the ScreenCast portal stream without X11 access. |
| `--socket=pulseaudio` | Playback and recording audio | H.264 and AV1 captures each contained 48 kHz stereo AAC from a controlled null-sink monitor; playback and FFmpeg decode succeeded. |
| `--device=dri` | GTK/GStreamer rendering and hardware encode/decode | GSR used the AMD render device and produced VAAPI H.264 and AV1. |
| `--talk-name=org.kde.StatusNotifierWatcher` | Register the retained SNI | Plasma registered one item with the stable/development icon, title/status, Open, and Quit. No other session name is granted. |
| `--filesystem=xdg-config/WarcraftRecorder:ro` | One-way import of Electron's known config | Existing `config-v3.json` and directory were readable but not writable; contents were not inspected. |

`xdg-run/pipewire-0` was tested and removed: a portal capture with audio negotiated and streamed successfully under a temporary explicit `--nofilesystem=xdg-run/pipewire-0` override. The portal-provided file descriptor and PulseAudio bridge are sufficient.

Folder authorization used three disposable host directories under `/home/johanw/wr002-folder-proof`. The portal persisted these document paths:

```text
wow-log-folder   /run/user/1000/doc/qDmuljuxPQ3c7XHNmZDWvg/wow-logs
recording-folder /run/user/1000/doc/FPryRUTpmZJJXAPUiDtCMg/recordings
replay-folder    /run/user/1000/doc/iuvTNOxxl9z6udINslMCDg/replays
```

After restart, `--folder-access-probe` reported `rust=true gsr=true ffmpeg=true` for all three. The unselected repository path `/home/johanw/repos/wow-recorder-linuxwayland/AGENTS.md` was invisible. The legacy config remained read-only and byte-identical before/after the platform probe (SHA-256 `b392a1a849916b6f7184080dafbe809bf4fced8ac2cf47f60ea0ea86de6c0c5f`). This ticket proves authorization and child-process access only: WR-003 owns imported-path authorization state, and WR-007 owns the rule that denied/unselected paths cannot start scanning or eviction.

## Pinned components, licenses, and payload

| Component | Exact pin | License | Installed contribution |
|---|---|---|---|
| GNOME runtime/SDK | branch `50` | runtime aggregate | Shared runtime, excluded from app payload. |
| Rust SDK extension | branch `25.08`, selected by GNOME 50 SDK | aggregate | Build-only. |
| Clapper native | tag `0.10.0`, SHA-256 `344c0f20e540a63c6fb44cdd5de88c168ed145bb66c1307e79b2b08124780118` | LGPL-2.1-or-later libraries/plugin; GPL utility notices also retained | `libclapper` 310,816 bytes; `libclapper-gtk` 182,696 bytes, plus plugin. |
| Clapper Rust bindings | crates `clapper-player`/`clapper-player-gtk` `0.10.1` | GPL-3.0-or-later | Statically represented in the Rust binary. |
| GSR | `5.13.9`, SHA-256 `a09365f687002c87f99bcaa4665be1bd0188fa4358f437368fc6a95ab50f4d78` | GPLv3 | 457,352-byte executable; KMS helper, systemd integration, app-audio helper, and examples omitted. |
| FFmpeg | `8.0.1`, SHA-256 `05ee0b03119b45c0bdb4df654b96802e909e0a752f72e4fe3794f487229e5a41` | configured GPLv3-or-later | 448,800-byte executable; FFmpeg application libraries total 9,542,464 bytes. |
| dav1d | GNOME 50 runtime `1.5.1` | BSD-2-Clause | Runtime-provided AV1 software decoder; no duplicate library bundled. |
| x264 | commit `b35605ace3ddf7c1a5d67a2eb553f034aef41d55` | GPL-2.0-or-later | 1,834,592-byte shared library. |
| nv-codec-headers | commit `e844e5b26f46bb77479f063029595293aa8f812d` (`n13.0.19.0`) | MIT-style per-file grants | Headers removed; the five source notices are retained. |
| `ksni` | crate `0.3.6`, defaults off, features `async-io`,`blocking` | Unlicense | One service thread with a shutdown handle whose completion is awaited; no exposed `JoinHandle`, no Tokio. |

`flatpak info --show-size` reports 17,672,192 bytes; `du -sh` reports 25 MiB allocated. The Rust executable is 3,741,856 bytes. There are nine direct Cargo dependencies and no Tokio dependency.

Installed notices include the app/GSR GPLv3 texts, Clapper LGPL/GPL texts, FFmpeg's relevant license set, x264 COPYING, and each nv-codec header grant.

## Capture proof

The controlled source was a disposable Warcraft Recorder window. Audio came from a named silent PulseAudio null sink (`wr002_null.monitor`), proving an actual audio stream without capturing user audio. Representative GSR arguments were:

```text
gpu-screen-recorder -w portal -restore-portal-session yes \
  -portal-session-token-filepath /var/data/wr002/portal-token \
  -r 8 -replay-storage ram -restart-replay-on-save no \
  -c mkv -f 30 -keyint 2 -bm cbr -q 5000 \
  -k h264 -ac aac -cursor no -a wr002_null.monitor \
  -sc /var/data/wr002/gsr-hook.sh \
  -o <authorized-replay-document-path> \
  -ro <authorized-recording-document-path>
```

`SIGUSR1` saved the replay; `SIGRTMIN` toggled regular recording; `SIGINT` shut down. The same command with `-k av1` proved AV1. Primary outputs:

| Output | SHA-256 | Probe result |
|---|---|---|
| H.264 replay `Replay_2026-07-19_23-44-31.mkv` | `20162459fa4faad0f94a3aa00949d02ca1abc96d05579973e69d95316219eba2` | 9.200 s; H.264 High, yuv420p BT.709, 1344x756, 30 fps; AAC-LC 48 kHz stereo. |
| H.264 regular `Video_2026-07-19_23-44-33.mkv` | `3e9c6caa96539edd40b7f16600f180fde0b5522822c138df3a53f0311d9bf023` | 4.010 s; H.264 High 1344x756 at 30 fps; AAC-LC 48 kHz stereo. |
| AV1 replay `Replay_2026-07-19_23-45-09.mkv` | `4eb7838950a8a4dea154c65f025f2bb5a9147e3490e38795154d50c164460696` | 9.766 s; AV1 Main, yuv420p BT.709, 1344x768, 30 fps; AAC-LC 48 kHz stereo. |
| AV1 regular `Video_2026-07-19_23-45-11.mkv` | `2be451f003bae48a0cce4206541ad7662a75d122920dcb7d9daf7c8a84558ef5` | 4.010 s; AV1 Main 1344x768 at 30 fps; AAC-LC 48 kHz stereo. |

After removing unused Opus support and performing the final clean install, a final restored-token H.264/AAC replay smoke test produced `Replay_2026-07-19_23-54-34.mkv`, SHA-256 `f1a6767bb7e24ce9603560b05dd0a078033554a8687126c23f72469963cb5364`: 4.800 s, 1344x756 at 30 fps, AAC-LC 48 kHz stereo.

The first consent saved a portal restore token. A second launch restored without a picker. Deleting only the disposable token forced reselection: cancel produced exit code 60 and the explicit message `desktop portal capture failed ... canceled by the user`; restarting and selecting the controlled window generated a different token and captured successfully.

The application-owned background probe additionally emitted both hook callbacks:

```text
replay  .../Replay_2026-07-19_23-39-59.mkv
regular .../Video_2026-07-19_23-40-01.mkv
```

It remained alive while the app window was hidden, recovered after tray activation, and on Quit completed PipeWire teardown before GSR was reaped and the SNI unregistered. The application-owned probe used the same AAC contract.

## Playback proof

The development-only `--player-probe <path>` embeds the real ClapperGtk widget through the retained concrete `PlayerBackend`. The same control sequence was exercised on the H.264 and AV1 replay files above:

| Operation | H.264 | AV1 |
|---|---|---|
| Open/play with AAC audio | pass, duration 9.200 s | pass, duration 9.766 s |
| Seek near start, middle, end | pass | pass |
| Ten rapid seeks | pass | pass |
| Mute, unmute, volume 50% | pass | pass |
| Speed 0.25x, 1x, 2x | pass | pass |
| Pause, previous-frame seek, next-frame advance | pass | pass |
| Stop, close, reopen | pass; position returned to 0 | pass; position returned to 0 |

There were no Clapper/GStreamer playback errors or crashes. AV1 playback used runtime codec support; FFmpeg transforms specifically use the minimal build's `libdav1d` decoder.

## FFmpeg transform proof

The build uses shared libraries with `--disable-autodetect --disable-everything --disable-network`. It retains file/pipe protocols; concat/Matroska/MOV demuxers; Matroska/MP4 muxers; AAC, H.264, HEVC, and libdav1d decoders; AAC, libx264, NVENC, VAAPI, and Vulkan encoders; and the exact trim/transition/audio filters (`trim`, `atrim`, `setpts`, `asetpts`, `fps`, `scale`, `pad`, `fade`, `afade`, `concat`, `format`, `aformat`, `aresample`, `amix`). Opus was removed after the contract correction because WR-000 requires AAC.

The stream-copy command used H.264/AAC input SHA-256 `20162459fa4faad0f94a3aa00949d02ca1abc96d05579973e69d95316219eba2`:

```text
ffmpeg -ss 1 -i Replay_2026-07-19_23-44-31.mkv -t 3 \
  -c:v copy -c:a copy -avoid_negative_ts make_zero -movflags +faststart \
  wr002-controlled-aac-clip.mp4
```

Result: 2,507,487 bytes, SHA-256 `357fc5bbccc0fa84c1c6f2e9b7badc7ea40e15e9a5d5d844b6d4d8af4f6fd874`; MP4, 4.010 s (keyframe-aligned stream-copy result), H.264 High 1344x756 at 30 fps plus AAC-LC 48 kHz stereo.

The kill-video proof used the retained `VideoProcessQueue.prepareKillVideoComplexFilter`/render command shape rather than a simplified concat. Its inputs were the H.264/AAC replay above (`20162459fa4faad0f94a3aa00949d02ca1abc96d05579973e69d95316219eba2`) and AV1/AAC replay `Replay_2026-07-19_23-45-09.mkv` (`4eb7838950a8a4dea154c65f025f2bb5a9147e3490e38795154d50c164460696`). The test selected the legacy `1024x768` canvas and `30` fps choices so aspect-preserving width scaling leaves both source heights within the canvas. Each source exercises `fps`, `scale=<width>:-2`, and centered `pad`; the filter graph retains the legacy one-second fades and spliced audio. The exact executed sandbox argv/filter graph was:

```text
flatpak run --command=ffmpeg io.github.JohanWes.WarcraftRecorder.Devel \
  -i /run/user/1000/doc/iuvTNOxxl9z6udINslMCDg/replays/Replay_2026-07-19_23-44-31.mkv \
  -i /run/user/1000/doc/iuvTNOxxl9z6udINslMCDg/replays/Replay_2026-07-19_23-45-09.mkv \
  -filter_complex \
  '[0:v]trim=start=0:end=3,setpts=PTS-STARTPTS,fps=30,scale=1024:-2,pad=1024:768:(ow-iw)/2:(oh-ih)/2,fade=t=in:st=0:d=1,fade=t=out:st=2:d=1[v0];[0:a]atrim=start=0:end=3,asetpts=PTS-STARTPTS,afade=t=in:st=0:d=1,afade=t=out:st=2:d=1[a0];[1:v]trim=start=0:end=3,setpts=PTS-STARTPTS,fps=30,scale=1024:-2,pad=1024:768:(ow-iw)/2:(oh-ih)/2,fade=t=in:st=0:d=1,fade=t=out:st=2:d=1[v1];[1:a]atrim=start=0:end=3,asetpts=PTS-STARTPTS,afade=t=in:st=0:d=1,afade=t=out:st=2:d=1[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[v][a]' \
  -movflags +faststart -map '[v]' -map '[a]' -shortest \
  -c:v libx264 -crf 22 -c:a aac -preset fast -pix_fmt yuv420p -xerror \
  /run/user/1000/doc/FPryRUTpmZJJXAPUiDtCMg/recordings/wr002-legacy-shape-aac-montage.mp4
```

The sandboxed shipped FFmpeg exited `0` with 180 output frames and mapped H.264, `libdav1d`, and both AAC inputs through the graph. Result: 27,142 bytes, SHA-256 `47c067d311e979b7f3c4bf59ef675865b6a4303f0f925af09f3a400fe01fd456`. Host `ffprobe -v error -show_entries format=format_name,duration,size:stream=index,codec_name,profile,codec_type,width,height,pix_fmt,color_space,color_transfer,color_primaries,r_frame_rate,sample_rate,channels -of json` reported MP4, exactly 6.000 s and 27,142 bytes; H.264 High, yuv420p BT.709, 1024x768 at 30 fps; and AAC-LC 48 kHz stereo. A second sandbox pass through the installed runtime's GStreamer playback stack (`gst-launch-1.0 -m playbin uri=file:///.../wr002-legacy-shape-aac-montage.mp4 video-sink=fakesink audio-sink=fakesink`) decoded both streams, reached `PLAYING`, received EOS, emitted no error, and exited `0`.

The first montage attempt exposed that FFmpeg's native AV1 decoder selected a hardware-only path on this AMD platform. Enabling the runtime-provided `libdav1d` decoder fixed the real gate without adding a duplicate codec library or general-purpose codec suite.

## Tray/background proof

| Scenario | Result |
|---|---|
| Watcher registration/menu | SNI registered with exact narrow bus grant; stable/development icon, title/status, Open, and Quit were available. |
| Window hide during capture | Close hid the window and left the real GSR replay buffer alive. |
| Open activation | SNI activation presented the existing window; capture remained alive. |
| Quit | Open uses non-blocking/coalescible delivery; Quit uses lossless bounded-channel delivery. After 64 concurrent Open activations, menu Quit was still received: the owned GSR child received `SIGINT`, completed PipeWire teardown, was waited/reaped on poll 2, then the shutdown-awaitable tray service unregistered. |
| Watcher absent | `dbus-run-session` created a session with the manifest's SNI permission unchanged but no `org.kde.StatusNotifierWatcher`. After the offline transition, closing the window exited the app with code 0; no invisible process remained. |

The saturation behavior also has a deterministic unit test: a capacity-one channel is filled with Open, Quit is sent from the service-side path, the consumer receives Open then Quit, and the sender completes. It contains no timing assertion.

## Commands and verification

```text
$ wr002_builder_dir=$(mktemp -d /tmp/wr002-builder-repro.XXXXXX)
$ curl -fsSL \
    https://mirror.zyner.org/mirror/cachyos/repo/x86_64_v4/cachyos-extra-znver4/flatpak-builder-1.4.10-1.1-x86_64_v4.pkg.tar.zst \
    -o "$wr002_builder_dir/pkg.tar.zst"
$ printf '%s  %s\n' \
    1417746f838128b54215cfe43525fcde337cb5fe02e86766825a23581f95870a \
    "$wr002_builder_dir/pkg.tar.zst" | sha256sum -c -
/tmp/wr002-builder-repro.Mp3mBH/pkg.tar.zst: OK
$ bsdtar -xf "$wr002_builder_dir/pkg.tar.zst" -C "$wr002_builder_dir"
$ "$wr002_builder_dir/usr/bin/flatpak-builder" --version
flatpak-builder-1.4.10

$ flatpak run --filesystem="$PWD" --command=flatpak-cargo-generator \
    org.flatpak.Builder native/Cargo.lock -o flatpak/cargo-sources.json
$ sha256sum flatpak/cargo-sources.json
b7262e54d88d210d04494eef4702756ae386a5abc4e1afb5fdf2945118b741b4

$ "$wr002_builder_dir/usr/bin/flatpak-builder" \
    --force-clean --user --install wr002-build \
    flatpak/io.github.JohanWes.WarcraftRecorder.Devel.yml
Finished `release` profile [optimized]
Composing metadata... Success!
Installing app/io.github.JohanWes.WarcraftRecorder.Devel/x86_64/master

$ cargo fmt --manifest-path native/Cargo.toml --check
<no output>
$ cargo clippy --manifest-path native/Cargo.toml --all-targets --all-features -- -D warnings
Finished `dev` profile
$ cargo test --manifest-path native/Cargo.toml --all-targets
running 1 test
test ui::tray_backend::tests::quit_waits_for_space_in_a_saturated_channel ... ok
test result: ok. 1 passed; 0 failed
$ cargo build --manifest-path native/Cargo.toml --release
Finished `release` profile
```

The host did not have `flatpak-builder` installed in `/usr/bin`; invoking the simplified command would return 127. `org.flatpak.Builder` also cannot build this manifest directly because its nested Flatpak installation cannot see the host user's GNOME 50 SDK. The standalone verified package above is the actual successful preparation/build path and was rerun from its newly extracted location. The Builder Flatpak is used only for linting and Cargo-source generation.

Clippy, tests, and the release build were run through the standalone builder's `--run` SDK sandbox stopped after the Clapper module, because the host does not install Clapper development files. This preserves the exact standard Cargo arguments while testing the pinned native ABI.

The genuine no-watcher command/result was:

```text
$ dbus-run-session -- sh -c '
    if busctl --user list --no-pager | grep -q org.kde.StatusNotifierWatcher; then
      echo watcher-present-unexpected; exit 90
    fi
    echo isolated-bus-has-no-status-notifier-watcher
    flatpak run io.github.JohanWes.WarcraftRecorder.Devel
    status=$?; echo no-watcher-app-exit=$status; exit $status'
isolated-bus-has-no-status-notifier-watcher
no-watcher-app-exit=0
```

```text
$ desktop-file-validate data/io.github.JohanWes.WarcraftRecorder.Devel.desktop
<no output>
$ appstreamcli validate --no-net data/io.github.JohanWes.WarcraftRecorder.Devel.metainfo.xml
Validation was successful: pedantic: 1
$ flatpak-builder-lint appstream ...metainfo.xml
Validation was successful: pedantic: 1
```

`flatpak-builder-lint manifest` was also run. It reports the three intentional ticket/product exceptions: Wayland-only without X11 fallback, the required exact read-only legacy-config grant, and the `.Devel` app ID not mapping to the repository URL inferred by the linter. No malformed metadata or undisclosed broad permission was reported.

## Decisions and deviations

- ADR-001 records the maintainer decision that newly authored native code is GPL-3.0-or-later while the legacy Electron tree remains unchanged until WR-013. This resolves compatibility with the GPL-3.0-or-later Clapper Rust bindings.
- GSR upstream always launches save hooks through `flatpak-spawn --host` when it detects Flatpak. Warcraft Recorder's hook is app-owned and sandbox-local; the pinned patch invokes it directly, avoiding the broad `org.freedesktop.Flatpak` permission.
- `ksni` defaults are disabled. The smallest blocking configuration is `async-io` plus `blocking`, giving one permitted service thread whose shutdown handle is awaited, without Tokio or an application-wide async runtime. The API does not expose a Rust `JoinHandle`, so this report does not call it joinable.
- Real GSR outputs substitute for absent WR-000 legacy sample files. This is the only sample-provenance deviation; codec, audio, player, and transform operations are real and sandboxed.
- WR-002 proves folder authorization, persistence, denial, replacement, and child visibility. The staged architecture intentionally defers imported-path authorization state to WR-003 and scan/eviction guards to WR-007; no nonexistent scanner behavior is claimed here. Under the maintainer-approved ticket sequence, the WR-002 platform gate remains `DONE` with that explicit ownership boundary.
- GNOME 50's exact EOL date is not yet published. The report records the published support boundary rather than a speculative date.

## Approval

- Implementer result: 2026-07-19 — `DONE`; all WR-002 acceptance gates passed.
- Reviewer: 2026-07-19 — `DONE`; independent re-review found no remaining issues and marked the ticket ready to merge.
- Maintainer: 2026-07-19 — approved under the standing authorization to proceed when review and verification pass.
