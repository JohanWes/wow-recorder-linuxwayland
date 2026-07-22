# Native tests

The maintained test surface is the Rust package and the fixture/golden corpus
under `tests/native/`. Run it from the repository root with:

```sh
cargo test --manifest-path native/Cargo.toml --all-targets
```

Legacy config and sidecar fixtures are intentionally retained for the native
one-way importer and compatible user tag/protection patch behavior. Generated
media and Flatpak build directories are disposable and must not be committed.
