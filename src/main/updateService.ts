import { app } from 'electron';
import { spawn } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import ConfigService from '../config/ConfigService';

export interface UpdateInfo {
  currentVersion: string;
  latestVersion: string;
  latestReleaseTag: string;
  currentReleaseTag: string;
  releaseUrl: string;
}

export const GITHUB_RELEASES_API =
  'https://api.github.com/repos/JohanWes/wow-recorder-linuxwayland/releases/latest';

export const INSTALL_SCRIPT_URL =
  'https://raw.githubusercontent.com/JohanWes/wow-recorder-linuxwayland/main/install.sh';

export function compareVersions(v1: string, v2: string): number {
  const clean = (v: string) => v.replace(/^v/, '').split('.').map(Number);
  const a = clean(v1);
  const b = clean(v2);

  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const x = a[i] || 0;
    const y = b[i] || 0;
    if (x > y) return 1;
    if (x < y) return -1;
  }

  return 0;
}

export function extractVersionFromTag(tag: string): string {
  return tag.replace(/^(linux-|v)/, '').replace(/-[0-9a-f]{7,}$/, '');
}

export function readInstalledReleaseTag(): string {
  const candidates = new Set<string>();
  const executablePath = process.execPath;

  if (executablePath) {
    const executableDir = path.dirname(executablePath);
    candidates.add(
      path.join(
        executableDir,
        '..',
        'share',
        'warcraftrecorder',
        'release-tag',
      ),
    );
  }

  candidates.add(
    path.join(
      os.homedir(),
      '.local',
      'share',
      'warcraftrecorder',
      'release-tag',
    ),
  );

  for (const candidate of candidates) {
    try {
      if (!fs.existsSync(candidate)) continue;
      return fs.readFileSync(candidate, 'utf8').trim();
    } catch (e) {
      console.warn(
        `[UpdateService] Failed to read installed release tag from ${candidate}:`,
        String(e),
      );
    }
  }

  return '';
}

export function getInstallPrefix(): string | null {
  const executableDir = path.dirname(process.execPath);

  if (path.basename(executableDir) !== 'bin') {
    return null;
  }

  return path.dirname(executableDir);
}

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, "'\\''")}'`;
}

export async function checkForUpdates(
  cfg: ConfigService,
): Promise<UpdateInfo | null> {
  const currentVersion = app.getVersion();

  if (process.env.WR_UPDATE_DRY_RUN === 'true') {
    console.info('[UpdateService] Dry run mode - simulating update available');
    return {
      currentVersion,
      latestVersion: `v${currentVersion}.999 (dry run)`,
      latestReleaseTag: `dry-run-${currentVersion}.999`,
      currentReleaseTag: readInstalledReleaseTag(),
      releaseUrl:
        'https://github.com/JohanWes/wow-recorder-linuxwayland/releases/latest',
    };
  }

  if (!app.isPackaged) {
    console.info('[UpdateService] Skipping update check in dev mode');
    return null;
  }

  try {
    console.info('[UpdateService] Checking for updates...');

    const response = await fetch(GITHUB_RELEASES_API, {
      headers: {
        Accept: 'application/vnd.github.v3+json',
        'User-Agent': 'WarcraftRecorder',
      },
    });

    if (!response.ok) {
      console.warn(
        `[UpdateService] GitHub API returned ${response.status} ${response.statusText}`,
      );
      return null;
    }

    const release = (await response.json()) as {
      tag_name: string;
      html_url: string;
    };

    const latestTag = release.tag_name;
    const latestVersion = extractVersionFromTag(latestTag);
    const currentReleaseTag = readInstalledReleaseTag();
    const versionComparison = compareVersions(latestVersion, currentVersion);

    console.info(
      `[UpdateService] Current: ${currentVersion} (${currentReleaseTag || 'unknown release tag'}), Latest: ${latestVersion} (${latestTag})`,
    );

    if (versionComparison < 0) {
      console.info('[UpdateService] Already up to date');
      return null;
    }

    if (versionComparison === 0 && currentReleaseTag === latestTag) {
      console.info('[UpdateService] Already on latest release tag');
      return null;
    }

    const dismissedReleaseTag = cfg.get<string>('dismissedUpdateVersion');
    if (dismissedReleaseTag === latestTag) {
      console.info(
        `[UpdateService] Release ${latestTag} was dismissed, skipping`,
      );
      return null;
    }

    console.info(`[UpdateService] Update available: ${latestVersion}`);
    return {
      currentVersion,
      latestVersion,
      latestReleaseTag: latestTag,
      currentReleaseTag,
      releaseUrl: release.html_url,
    };
  } catch (e) {
    console.warn('[UpdateService] Failed to check for updates:', String(e));
    return null;
  }
}

export function performUpdate(): Promise<void> {
  return new Promise((resolve, reject) => {
    console.info('[UpdateService] Running install script...');

    const installPrefix = getInstallPrefix();
    const installArgs = installPrefix
      ? ` -s -- --prefix ${shellQuote(installPrefix)}`
      : '';

    const child = spawn(
      'bash',
      ['-c', `curl -fsSL '${INSTALL_SCRIPT_URL}' | bash${installArgs}`],
      {
        stdio: 'pipe',
      },
    );

    let stdout = '';
    let stderr = '';

    child.stdout?.on('data', (data: Buffer) => {
      stdout += data.toString();
    });

    child.stderr?.on('data', (data: Buffer) => {
      stderr += data.toString();
    });

    child.on('close', (code) => {
      if (code === 0) {
        console.info('[UpdateService] Install script completed successfully');
        resolve();
      } else {
        const msg = stderr || stdout || `exit code ${code}`;
        console.error('[UpdateService] Install script failed:', msg);
        reject(new Error(msg.trim()));
      }
    });

    child.on('error', (err) => {
      console.error('[UpdateService] Failed to spawn install script:', err);
      reject(err);
    });
  });
}
