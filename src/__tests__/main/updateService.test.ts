jest.mock('electron', () => ({
  app: {
    getVersion: jest.fn(() => '1.0.0'),
    isPackaged: false,
  },
}));

import fs from 'fs';
import type ConfigService from '../../config/ConfigService';
import {
  checkForUpdates,
  compareVersions,
  extractVersionFromTag,
  GITHUB_RELEASES_API,
} from '../../main/updateService';

const mockApp = jest.requireMock('electron').app;

const mockConfig = (dismissedUpdateVersion = '') =>
  ({
    get: jest.fn((key: string) =>
      key === 'dismissedUpdateVersion' ? dismissedUpdateVersion : undefined,
    ),
  }) as unknown as ConfigService;

describe('compareVersions', () => {
  it('returns 1 when v1 > v2', () => {
    expect(compareVersions('1.2.3', '1.2.2')).toBe(1);
    expect(compareVersions('2.0.0', '1.9.9')).toBe(1);
    expect(compareVersions('1.2.3', '1.2.0')).toBe(1);
  });

  it('returns -1 when v1 < v2', () => {
    expect(compareVersions('1.2.2', '1.2.3')).toBe(-1);
    expect(compareVersions('1.9.9', '2.0.0')).toBe(-1);
    expect(compareVersions('1.0.0', '1.0.1')).toBe(-1);
  });

  it('returns 0 when versions are equal', () => {
    expect(compareVersions('1.2.3', '1.2.3')).toBe(0);
  });

  it('handles leading v prefix', () => {
    expect(compareVersions('v1.2.3', '1.2.3')).toBe(0);
    expect(compareVersions('1.2.3', 'v1.2.3')).toBe(0);
    expect(compareVersions('v1.2.3', 'v1.2.3')).toBe(0);
  });

  it('handles different length versions', () => {
    expect(compareVersions('1.2', '1.2.0')).toBe(0);
    expect(compareVersions('1.2.0.1', '1.2.0')).toBeGreaterThan(0);
    expect(compareVersions('1.2.0', '1.2.0.1')).toBeLessThan(0);
    expect(compareVersions('1', '1.0.0')).toBe(0);
    expect(compareVersions('2', '1.9.9')).toBe(1);
  });

  it('handles minor/patch differences', () => {
    expect(compareVersions('3.14.0', '3.13.2')).toBe(1);
    expect(compareVersions('3.14.0', '3.14.1')).toBe(-1);
    expect(compareVersions('3.14.1', '3.14.0')).toBe(1);
  });
});

describe('extractVersionFromTag', () => {
  it('strips linux- prefix and trailing sha suffix', () => {
    expect(extractVersionFromTag('linux-7.7.1-48a40a0')).toBe('7.7.1');
    expect(extractVersionFromTag('linux-7.7.1-48a40a087021b8cd')).toBe('7.7.1');
  });

  it('strips v prefix', () => {
    expect(extractVersionFromTag('v7.7.1')).toBe('7.7.1');
  });

  it('returns plain semver unchanged', () => {
    expect(extractVersionFromTag('7.7.1')).toBe('7.7.1');
  });

  it('compares linux-tag version against plain semver correctly', () => {
    expect(
      compareVersions(extractVersionFromTag('linux-7.7.1-48a40a0'), '7.4.0'),
    ).toBe(1);
    expect(
      compareVersions(extractVersionFromTag('linux-7.7.1-48a40a0'), '7.7.1'),
    ).toBe(0);
    expect(
      compareVersions(extractVersionFromTag('linux-7.4.0-48a40a0'), '7.7.1'),
    ).toBe(-1);
  });
});

describe('checkForUpdates - dry run mode', () => {
  const originalEnv = process.env;

  beforeEach(() => {
    process.env = { ...originalEnv };
  });

  afterEach(() => {
    process.env = originalEnv;
  });

  it('returns simulated update when WR_UPDATE_DRY_RUN is set', async () => {
    process.env.WR_UPDATE_DRY_RUN = 'true';

    const { checkForUpdates } = await import('../../main/updateService');
    const ConfigService = (await import('config/ConfigService')).default;

    const cfg = ConfigService.getInstance();
    const result = await checkForUpdates(cfg);

    expect(result).not.toBeNull();
    expect(result!.latestVersion).toContain('(dry run)');
    expect(result!.releaseUrl).toContain('github.com');
  });

  it('skips dry run when env is not set', async () => {
    delete process.env.WR_UPDATE_DRY_RUN;

    const { checkForUpdates } = await import('../../main/updateService');
    const ConfigService = (await import('config/ConfigService')).default;

    const cfg = ConfigService.getInstance();
    const result = await checkForUpdates(cfg);

    expect(result).toBeNull();
  });
});

describe('checkForUpdates - config integration', () => {
  it('exported constants are correct', () => {
    expect(GITHUB_RELEASES_API).toBe(
      'https://api.github.com/repos/JohanWes/wow-recorder-linuxwayland/releases/latest',
    );
  });
});

describe('checkForUpdates - release tag comparison', () => {
  const originalFetch = global.fetch;

  beforeEach(() => {
    mockApp.isPackaged = true;
    mockApp.getVersion.mockReturnValue('7.7.1');
    jest
      .spyOn(fs, 'existsSync')
      .mockImplementation((candidate) =>
        String(candidate).endsWith('/release-tag'),
      );
  });

  afterEach(() => {
    global.fetch = originalFetch;
    jest.restoreAllMocks();
  });

  it('returns null when the installed release tag already matches latest', async () => {
    jest.spyOn(fs, 'readFileSync').mockReturnValue('linux-7.7.1-639b6f5\n');
    global.fetch = jest.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        tag_name: 'linux-7.7.1-639b6f5',
        html_url:
          'https://github.com/JohanWes/wow-recorder-linuxwayland/releases/tag/linux-7.7.1-639b6f5',
      }),
    } as Response);

    await expect(checkForUpdates(mockConfig())).resolves.toBeNull();
  });

  it('prompts when the version is the same but the release sha changed', async () => {
    jest.spyOn(fs, 'readFileSync').mockReturnValue('linux-7.7.1-48a40a0\n');
    global.fetch = jest.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        tag_name: 'linux-7.7.1-639b6f5',
        html_url:
          'https://github.com/JohanWes/wow-recorder-linuxwayland/releases/tag/linux-7.7.1-639b6f5',
      }),
    } as Response);

    const result = await checkForUpdates(mockConfig());

    expect(result).toMatchObject({
      currentVersion: '7.7.1',
      latestVersion: '7.7.1',
      currentReleaseTag: 'linux-7.7.1-48a40a0',
      latestReleaseTag: 'linux-7.7.1-639b6f5',
    });
  });

  it('suppresses only the dismissed release tag', async () => {
    jest.spyOn(fs, 'readFileSync').mockReturnValue('linux-7.7.1-48a40a0\n');
    global.fetch = jest.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        tag_name: 'linux-7.7.1-639b6f5',
        html_url:
          'https://github.com/JohanWes/wow-recorder-linuxwayland/releases/tag/linux-7.7.1-639b6f5',
      }),
    } as Response);

    await expect(
      checkForUpdates(mockConfig('linux-7.7.1-639b6f5')),
    ).resolves.toBeNull();
  });
});
