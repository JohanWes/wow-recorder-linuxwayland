import { ConfigurationSchema } from 'config/configSchema';

export interface SettingsWarnings {
  storagePathMissing: boolean;
  noFlavourEnabled: boolean;
  enabledFlavourMissingLogPath: boolean;
  logPathMissing: boolean;
  needsAttention: boolean;
}

export const getSettingsWarnings = (
  config: ConfigurationSchema,
): SettingsWarnings => {
  const storagePathMissing = !config.storagePath;

  const noFlavourEnabled =
    !config.recordRetail &&
    !config.recordRetailPtr &&
    !config.recordClassic &&
    !config.recordClassicPtr &&
    !config.recordEra;

  const enabledFlavourMissingLogPath =
    (config.recordRetail && !config.retailLogPath) ||
    (config.recordRetailPtr && !config.retailPtrLogPath) ||
    (config.recordClassic && !config.classicLogPath) ||
    (config.recordClassicPtr && !config.classicPtrLogPath) ||
    (config.recordEra && !config.eraLogPath);

  const logPathMissing = noFlavourEnabled || enabledFlavourMissingLogPath;

  return {
    storagePathMissing,
    noFlavourEnabled,
    enabledFlavourMissingLogPath,
    logPathMissing,
    needsAttention: storagePathMissing || logPathMissing,
  };
};
