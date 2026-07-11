# Contributing

Install dependencies and start the Tauri application:

```bash
npm install
npm run tauri dev
```

Before submitting a change, run:

```bash
npm run typecheck
npm run lint
npm run build
cd src-tauri
cargo check
cargo test
```

Create a production desktop build with `npm run tauri build`.
