/* eslint-disable no-console */
const { spawnSync } = require('child_process');

const isWindows = process.platform === 'win32';

const run = (cmd, args) => {
  // On Windows, lifecycle scripts often need a shell to execute `.cmd` shims
  // from `node_modules/.bin` (e.g. `electron-builder.cmd`, `npm.cmd`).
  const res = spawnSync(cmd, args, { stdio: 'inherit', shell: isWindows });
  if (res.error) throw res.error;
  if (res.status !== 0) process.exit(res.status ?? 1);
};

// Windows builds rely on native deps living in `release/app`.
if (isWindows) {
  console.info('[postinstall] Installing production deps for release/app');
  run('electron-builder', ['install-app-deps']);
} else {
  console.info(
    `[postinstall] Skipping electron-builder install-app-deps on ${process.platform}`,
  );
}

// Always build the renderer DLL (used by dev mode).
console.info('[postinstall] Building renderer DLL');
run('npm', ['run', 'build:dll']);
