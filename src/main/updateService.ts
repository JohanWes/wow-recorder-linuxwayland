import { app } from 'electron';
import { spawn } from 'child_process';
import ConfigService from '../config/ConfigService';

export interface UpdateInfo {
  currentVersion: string;
  latestVersion: string;
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

export async function checkForUpdates(
  cfg: ConfigService,
): Promise<UpdateInfo | null> {
  const currentVersion = app.getVersion();

  if (process.env.WR_UPDATE_DRY_RUN === 'true') {
    console.info('[UpdateService] Dry run mode - simulating update available');
    return {
      currentVersion,
      latestVersion: `v${currentVersion}.999 (dry run)`,
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
    const latestVersion = latestTag.replace(/^v/, '');

    console.info(
      `[UpdateService] Current: ${currentVersion}, Latest: ${latestVersion}`,
    );

    if (compareVersions(latestVersion, currentVersion) <= 0) {
      console.info('[UpdateService] Already up to date');
      return null;
    }

    const dismissedVersion = cfg.get<string>('dismissedUpdateVersion');
    if (dismissedVersion === latestVersion) {
      console.info(
        `[UpdateService] Version ${latestVersion} was dismissed, skipping`,
      );
      return null;
    }

    console.info(`[UpdateService] Update available: ${latestVersion}`);
    return {
      currentVersion,
      latestVersion,
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

    const child = spawn(
      'bash',
      ['-c', `curl -fsSL '${INSTALL_SCRIPT_URL}' | bash`],
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
