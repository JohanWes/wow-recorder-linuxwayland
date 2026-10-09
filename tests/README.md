# Native tests

Run the Rust tests from the repository root:

```sh
cargo test --manifest-path native/Cargo.toml --all-targets
```

- `native/bin/`: shell fakes for `gpu-screen-recorder` and `ffmpeg`, used by
  the recorder, media-job, and vertical-slice tests.
- `native/fixtures/legacy/sidecars/`: sidecars written by the old Electron
  app, kept for the legacy import and tag/protect patch tests.
