import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  checkAdvancedCombatLogging,
  getConfigWtfPath,
} from '../../main/util';

describe('Config.wtf helpers', () => {
  let tempDir: string;
  let logPath: string;
  let configWtfPath: string;

  beforeEach(async () => {
    tempDir = await fs.promises.mkdtemp(path.join(os.tmpdir(), 'wcr-util-'));
    logPath = path.join(tempDir, '_retail_', 'Logs');
    configWtfPath = path.join(tempDir, '_retail_', 'WTF', 'Config.wtf');
    await fs.promises.mkdir(logPath, { recursive: true });
  });

  afterEach(async () => {
    await fs.promises.rm(tempDir, { recursive: true, force: true });
  });

  it('resolves Config.wtf next to a configured Logs directory', () => {
    expect(getConfigWtfPath(logPath)).toBe(configWtfPath);
  });

  it('returns true when advanced combat logging is enabled', async () => {
    await fs.promises.mkdir(path.dirname(configWtfPath), { recursive: true });
    await fs.promises.writeFile(
      configWtfPath,
      'SET advancedCombatLogging "1"\n',
    );

    await expect(checkAdvancedCombatLogging(logPath)).resolves.toBe(true);
  });

  it('returns false when advanced combat logging is disabled', async () => {
    await fs.promises.mkdir(path.dirname(configWtfPath), { recursive: true });
    await fs.promises.writeFile(
      configWtfPath,
      'SET advancedCombatLogging "0"\n',
    );

    await expect(checkAdvancedCombatLogging(logPath)).resolves.toBe(false);
  });

  it('returns false when Config.wtf is missing', async () => {
    await expect(checkAdvancedCombatLogging(logPath)).resolves.toBe(false);
  });

  it('returns false when Config.wtf does not contain the setting', async () => {
    await fs.promises.mkdir(path.dirname(configWtfPath), { recursive: true });
    await fs.promises.writeFile(configWtfPath, 'SET locale "enUS"\n');

    await expect(checkAdvancedCombatLogging(logPath)).resolves.toBe(false);
  });
});
