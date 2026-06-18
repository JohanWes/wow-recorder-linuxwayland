import { ConfigurationSchema } from 'config/configSchema';
import { getSettingsWarnings } from '../../renderer/settingsWarnings';

const makeConfig = (
  overrides: Partial<ConfigurationSchema> = {},
): ConfigurationSchema =>
  ({
    storagePath: '',
    retailLogPath: '',
    retailPtrLogPath: '',
    classicLogPath: '',
    classicPtrLogPath: '',
    eraLogPath: '',
    recordRetail: false,
    recordClassic: false,
    recordClassicPtr: false,
    recordRetailPtr: false,
    recordEra: false,
    ...overrides,
  }) as ConfigurationSchema;

describe('getSettingsWarnings', () => {
  it('flags all warnings for a fresh empty config', () => {
    const warnings = getSettingsWarnings(makeConfig());
    expect(warnings.storagePathMissing).toBe(true);
    expect(warnings.noFlavourEnabled).toBe(true);
    expect(warnings.enabledFlavourMissingLogPath).toBe(false);
    expect(warnings.logPathMissing).toBe(true);
    expect(warnings.needsAttention).toBe(true);
  });

  it('clears storage warning when storagePath is set', () => {
    const warnings = getSettingsWarnings(
      makeConfig({ storagePath: '/home/user/wow-videos' }),
    );
    expect(warnings.storagePathMissing).toBe(false);
    expect(warnings.noFlavourEnabled).toBe(true);
    expect(warnings.logPathMissing).toBe(true);
    expect(warnings.needsAttention).toBe(true);
  });

  it('clears log-path warnings when a flavour is enabled with its log path set', () => {
    const warnings = getSettingsWarnings(
      makeConfig({
        storagePath: '/home/user/wow-videos',
        recordRetail: true,
        retailLogPath: '/home/user/wow/_retail_/Logs',
      }),
    );
    expect(warnings.storagePathMissing).toBe(false);
    expect(warnings.noFlavourEnabled).toBe(false);
    expect(warnings.enabledFlavourMissingLogPath).toBe(false);
    expect(warnings.logPathMissing).toBe(false);
    expect(warnings.needsAttention).toBe(false);
  });

  it('warns when a flavour is enabled but its log path is empty', () => {
    const warnings = getSettingsWarnings(
      makeConfig({
        storagePath: '/home/user/wow-videos',
        recordClassic: true,
        classicLogPath: '',
      }),
    );
    expect(warnings.storagePathMissing).toBe(false);
    expect(warnings.noFlavourEnabled).toBe(false);
    expect(warnings.enabledFlavourMissingLogPath).toBe(true);
    expect(warnings.logPathMissing).toBe(true);
    expect(warnings.needsAttention).toBe(true);
  });

  it('detects Classic PTR flavour with missing log path', () => {
    const warnings = getSettingsWarnings(
      makeConfig({
        storagePath: '/home/user/wow-videos',
        recordClassicPtr: true,
        classicPtrLogPath: '',
      }),
    );
    expect(warnings.enabledFlavourMissingLogPath).toBe(true);
    expect(warnings.logPathMissing).toBe(true);
  });

  it('detects Era flavour with missing log path', () => {
    const warnings = getSettingsWarnings(
      makeConfig({
        storagePath: '/home/user/wow-videos',
        recordEra: true,
        eraLogPath: '',
      }),
    );
    expect(warnings.enabledFlavourMissingLogPath).toBe(true);
    expect(warnings.logPathMissing).toBe(true);
  });

  it('detects Retail PTR flavour with missing log path', () => {
    const warnings = getSettingsWarnings(
      makeConfig({
        storagePath: '/home/user/wow-videos',
        recordRetailPtr: true,
        retailPtrLogPath: '',
      }),
    );
    expect(warnings.enabledFlavourMissingLogPath).toBe(true);
    expect(warnings.logPathMissing).toBe(true);
  });

  it('returns all false when everything is configured', () => {
    const warnings = getSettingsWarnings(
      makeConfig({
        storagePath: '/home/user/wow-videos',
        recordRetail: true,
        retailLogPath: '/home/user/wow/_retail_/Logs',
        recordClassic: true,
        classicLogPath: '/home/user/wow/_classic_/Logs',
      }),
    );
    expect(warnings.storagePathMissing).toBe(false);
    expect(warnings.noFlavourEnabled).toBe(false);
    expect(warnings.enabledFlavourMissingLogPath).toBe(false);
    expect(warnings.logPathMissing).toBe(false);
    expect(warnings.needsAttention).toBe(false);
  });
});
