declare module 'noobs' {
  export type ObsProperty = {
    name: string;
    type: string;
    items?: unknown[];
    currentValue?: unknown;
    value?: unknown;
  };
  export type SceneItemPosition = unknown;
  export type SourceDimensions = unknown;
  export type ObsData = unknown;
  export type Signal = unknown;
  export type ObsListItem = unknown;

  const noobs: Record<string, (...args: unknown[]) => void>;
  export default noobs;
}
