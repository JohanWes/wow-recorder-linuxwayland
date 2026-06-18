type ObsProperty = {
  name: string;
  type: string;
  items?: unknown[];
  currentValue?: unknown;
  value?: unknown;
};
type SceneItemPosition = unknown;
type SourceDimensions = unknown;
type ObsData = unknown;
type Signal = unknown;
type ObsListItem = unknown;

const fail = (fn: string) => {
  throw new Error(`noobs is not available on this platform (called ${fn})`);
};

const noobs = {
  SetVolmeterEnabled: () => fail('SetVolmeterEnabled'),
  GetPreviewInfo: () => fail('GetPreviewInfo'),
  ResetVideoContext: () => fail('ResetVideoContext'),
  SetRecordingCfg: () => fail('SetRecordingCfg'),
  SetVideoEncoder: () => fail('SetVideoEncoder'),
  StartBuffer: () => fail('StartBuffer'),
  StartRecording: () => fail('StartRecording'),
  StopRecording: () => fail('StopRecording'),
  ForceStopRecording: () => fail('ForceStopRecording'),
  GetLastRecording: () => fail('GetLastRecording'),
  GetSourceSettings: () => fail('GetSourceSettings'),
  SetSourceSettings: () => fail('SetSourceSettings'),
  GetSourceProperties: () => fail('GetSourceProperties'),
  CreateSource: () => fail('CreateSource'),
  DeleteSource: () => fail('DeleteSource'),
  AddSourceToScene: () => fail('AddSourceToScene'),
  RemoveSourceFromScene: () => fail('RemoveSourceFromScene'),
  SetSourceVolume: () => fail('SetSourceVolume'),
  SetForceMono: () => fail('SetForceMono'),
  SetAudioSuppression: () => fail('SetAudioSuppression'),
};

export default noobs;
export {
  ObsProperty,
  SceneItemPosition,
  SourceDimensions,
  ObsData,
  Signal,
  ObsListItem,
};
