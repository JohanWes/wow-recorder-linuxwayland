import {
  BaseConfig,
  ObsVideoConfig,
  ObsAudioConfig,
  ObsOverlayConfig,
  AudioSource,
} from 'main/types';
import path from 'path';
import ConfigService from '../config/ConfigService';
import { categoryRecordingSettings } from '../main/constants';
import { VideoCategory } from '../types/VideoCategory';
import { ESupportedEncoders } from '../main/obsEnums';
import { Language, Phrase } from 'localisation/phrases';
import { getLocalePhrase } from 'localisation/translations';
import {
  checkDisk,
  exists,
  getWowFlavour,
  isFolderOwned,
  takeOwnershipBufferDir,
  takeOwnershipStorageDir,
} from 'main/util';

const allowRecordCategory = (cfg: ConfigService, category: VideoCategory) => {
  if (category === VideoCategory.Clips) {
    console.info('[configUtils] Clips are never recorded directly');
    return false;
  }

  const categoryConfig = categoryRecordingSettings[category];

  if (!categoryConfig) {
    console.info('[configUtils] Unrecognised category', category);
    return false;
  }

  const categoryAllowed = cfg.get<boolean>(categoryConfig.allowRecordKey);

  if (!categoryAllowed) {
    console.info('[configUtils] Configured to not record:', category);
    return false;
  }

  console.info('[configUtils] Good to record:', category);
  return true;
};

const getBaseConfig = (cfg: ConfigService): BaseConfig => {
  const storagePath = cfg.getPath('storagePath');
  let obsPath: string;

  if (cfg.get<boolean>('separateBufferPath')) {
    obsPath = cfg.getPath('bufferStoragePath');
  } else {
    obsPath = path.join(storagePath, '.temp');
  }

  // Bizzare here bug where the amd_amf_h264 suddenly started massively leaking
  // memory for me, and switching to the texture one resolved it. This migrates
  // all users of the amd_amf_h264 encoder to use h264_texture_amf.
  let obsRecEncoder = cfg.get<string>('obsRecEncoder');

  if (obsRecEncoder === 'amd_amf_h264') {
    obsRecEncoder = ESupportedEncoders.AMD_H264;
    cfg.set('obsRecEncoder', obsRecEncoder);
  }

  // The jim_nvenc encoder was deprecated in libobs. Migrate to the new version.
  if (obsRecEncoder === 'jim_nvenc') {
    obsRecEncoder = ESupportedEncoders.NVENC_H264;
    cfg.set('obsRecEncoder', obsRecEncoder);
  }

  // The jim_av1_nvenc encoder was deprecated in libobs. Migrate to the new version.
  if (obsRecEncoder === 'jim_av1_nvenc') {
    obsRecEncoder = ESupportedEncoders.NVENC_AV1;
    cfg.set('obsRecEncoder', obsRecEncoder);
  }

  return {
    storagePath: cfg.get<string>('storagePath'),
    maxStorage: cfg.get<number>('maxStorage'),
    obsPath,
    obsOutputResolution: cfg.get<string>('obsOutputResolution'),
    obsFPS: cfg.get<number>('obsFPS'),
    obsQuality: cfg.get<string>('obsQuality'),
    obsRecEncoder,
    recordClassic: cfg.get<boolean>('recordClassic'),
    recordClassicPtr: cfg.get<boolean>('recordClassicPtr'),
    classicLogPath: cfg.get<string>('classicLogPath'),
    classicPtrLogPath: cfg.get<string>('classicPtrLogPath'),
    recordRetail: cfg.get<boolean>('recordRetail'),
    recordRetailPtr: cfg.get<boolean>('recordRetailPtr'),
    retailLogPath: cfg.get<string>('retailLogPath'),
    recordEra: cfg.get<boolean>('recordEra'),
    eraLogPath: cfg.get<string>('eraLogPath'),
    retailPtrLogPath: cfg.get<string>('retailPtrLogPath'),
    validateLogPaths: cfg.get<boolean>('validateLogPaths'),
  };
};

const getObsVideoConfig = (cfg: ConfigService): ObsVideoConfig => {
  return {
    obsCaptureMode: cfg.get<string>('obsCaptureMode'),
    monitorIndex: cfg.get<number>('monitorIndex'),
    captureCursor: cfg.get<boolean>('captureCursor'),
    forceSdr: cfg.get<boolean>('forceSdr'),
    videoSourceScale: cfg.get<number>('videoSourceScale'),
    videoSourceXPosition: cfg.get<number>('videoSourceXPosition'),
    videoSourceYPosition: cfg.get<number>('videoSourceYPosition'),
  };
};

const getObsAudioConfig = (cfg: ConfigService): ObsAudioConfig => {
  return {
    audioSources: cfg.get<AudioSource[]>('audioSources'),
    obsAudioSuppression: cfg.get<boolean>('obsAudioSuppression'),
    obsForceMono: cfg.get<boolean>('obsForceMono'),
    pushToTalk: cfg.get<boolean>('pushToTalk'),
    pushToTalkKey: cfg.get<number>('pushToTalkKey'),
    pushToTalkMouseButton: cfg.get<number>('pushToTalkMouseButton'),
    pushToTalkModifiers: cfg.get<string>('pushToTalkModifiers'),
  };
};

const getOverlayConfig = (cfg: ConfigService): ObsOverlayConfig => {
  return {
    chatOverlayEnabled: cfg.get<boolean>('chatOverlayEnabled'),
    chatOverlayOwnImage: cfg.get<boolean>('chatOverlayOwnImage'),
    chatOverlayOwnImagePath: cfg.get<string>('chatOverlayOwnImagePath'),
    chatOverlayScale: cfg.get<number>('chatOverlayScale'),
    chatOverlayXPosition: cfg.get<number>('chatOverlayXPosition'),
    chatOverlayYPosition: cfg.get<number>('chatOverlayYPosition'),
    chatOverlayCropX: cfg.get<number>('chatOverlayCropX'),
    chatOverlayCropY: cfg.get<number>('chatOverlayCropY'),
  };
};

const getLocaleError = (phrase: Phrase) => {
  const lang = ConfigService.getInstance().get<string>('language') as Language;
  return getLocalePhrase(lang, phrase);
};

const validateBaseConfig = async (config: BaseConfig) => {
  const {
    storagePath,
    maxStorage,
    obsPath,
    recordRetail,
    retailLogPath,
    recordRetailPtr,
    retailPtrLogPath,
    recordClassic,
    recordClassicPtr,
    classicLogPath,
    classicPtrLogPath,
    recordEra,
    eraLogPath,
    validateLogPaths,
  } = config;

  if (!storagePath) {
    console.warn('[Manager] storagePath is falsy', storagePath);
    const error = getLocaleError(Phrase.ErrorStoragePathInvalid);
    throw new Error(error);
  }

  if (storagePath.includes('#')) {
    // A user hit this: the video player loads the file path as a URL where
    // # is interpreted as a timestamp.
    console.warn('[Manager] storagePath contains #', storagePath);
    const error = getLocaleError(Phrase.ErrorStoragePathInvalid);
    throw new Error(error);
  }

  const storagePathExists = await exists(storagePath);

  if (!storagePathExists) {
    console.warn('[Manager] storagePath does not exist', storagePath);
    const error = getLocaleError(Phrase.ErrorStoragePathInvalid);
    throw new Error(error);
  }

  await checkDisk(storagePath, maxStorage);

  if (!obsPath) {
    console.warn('[Manager] obsPath is falsy', obsPath);
    const error = getLocaleError(Phrase.ErrorBufferPathInvalid);
    throw new Error(error);
  }

  const obsParentDir = path.dirname(obsPath);
  const obsParentDirExists = await exists(obsParentDir);

  if (!obsParentDirExists) {
    console.warn('[Manager] obsPath does not exist', obsPath);
    const error = getLocaleError(Phrase.ErrorBufferPathInvalid);
    throw new Error(error);
  }

  if (path.resolve(storagePath) === path.resolve(obsPath)) {
    console.warn('[Manager] storagePath is the same as obsPath');
    const error = getLocaleError(Phrase.ErrorStoragePathSameAsBufferPath);
    throw new Error(error);
  }

  const obsDirExists = await exists(obsPath);

  // 10GB is a rough guess at what the worst case buffer directory might be.
  if (obsDirExists) {
    await checkDisk(obsPath, 10);
  } else {
    const parentDir = path.dirname(obsPath);
    await checkDisk(parentDir, 10);
  }

  const storagePathOwned = await isFolderOwned(storagePath);

  if (!storagePathOwned) {
    await takeOwnershipStorageDir(storagePath);
  }

  if (obsDirExists && !(await isFolderOwned(obsPath))) {
    await takeOwnershipBufferDir(obsPath);
  }

  if (recordRetail) {
    const validRetailFlavours = ['wow'];

    const validFlavor = validateLogPaths
      ? validRetailFlavours.includes(getWowFlavour(retailLogPath))
      : true;

    const validPath = path.basename(retailLogPath) === 'Logs';
    const valid = validFlavor && validPath;

    if (!valid) {
      console.error('[Util] Invalid retail log path', retailLogPath);
      const error = getLocaleError(Phrase.InvalidRetailLogPath);
      throw new Error(error);
    }
  }

  if (recordRetailPtr) {
    const validRetailPtrFlavours = ['wowxptr', 'wow_beta'];

    const validFlavor = validateLogPaths
      ? validRetailPtrFlavours.includes(getWowFlavour(retailPtrLogPath))
      : true;

    const validPath = path.basename(retailPtrLogPath) === 'Logs';
    const valid = validFlavor && validPath;

    if (!valid) {
      console.error('[Util] Invalid retail PTR log path', retailPtrLogPath);
      const error = getLocaleError(Phrase.InvalidRetailPtrLogPathText);
      throw new Error(error);
    }
  }

  if (recordClassic) {
    const validClassicFlavours = ['wow_classic'];

    const validFlavour = validateLogPaths
      ? validClassicFlavours.includes(getWowFlavour(classicLogPath))
      : true;

    const validPath = path.basename(classicLogPath) === 'Logs';
    const valid = validFlavour && validPath;

    if (!valid) {
      console.error('[Util] Invalid classic log path', classicLogPath);
      const error = getLocaleError(Phrase.InvalidClassicLogPath);
      throw new Error(error);
    }
  }

  if (recordClassicPtr) {
    const validClassicPtrFlavours = ['wow_classic_beta', 'wow_classic_ptr'];
    const validFlavor = validateLogPaths
      ? validClassicPtrFlavours.includes(getWowFlavour(classicPtrLogPath))
      : true;

    const validPath = path.basename(classicPtrLogPath) === 'Logs';
    const valid = validFlavor && validPath;

    if (!valid) {
      console.error('[Util] Invalid classic PTR log path', classicPtrLogPath);
      const error = getLocaleError(Phrase.InvalidClassicPtrLogPath);
      throw new Error(error);
    }
  }

  if (recordEra) {
    const validEraFlavours = ['wow_classic_era', 'wow_classic_era_ptr'];

    const validFlavour = validateLogPaths
      ? validEraFlavours.includes(getWowFlavour(eraLogPath))
      : true;

    const validPath = path.basename(eraLogPath) === 'Logs';
    const valid = validFlavour && validPath;

    if (!valid) {
      console.error('[Util] Invalid era log path', eraLogPath);
      const error = getLocaleError(Phrase.InvalidEraLogPath);
      throw new Error(error);
    }
  }
};

export {
  allowRecordCategory,
  getBaseConfig,
  getObsVideoConfig,
  getObsAudioConfig,
  getOverlayConfig,
  validateBaseConfig,
  getLocaleError,
};
