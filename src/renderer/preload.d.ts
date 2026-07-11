import type { RendererVideo } from 'main/types';

export type RendererChannel = string;

declare global {
  interface Window {
    electron: {
      platform: string;
      ipcRenderer: {
        sendMessage(channel: RendererChannel, args?: unknown[]): void;
        sendSync(channel: RendererChannel, args: unknown[]): unknown;
        invoke(channel: RendererChannel, args?: unknown[]): Promise<unknown>;
        on(channel: string, func: (...args: unknown[]) => void): (() => void) | undefined;
        once(channel: string, func: (...args: unknown[]) => void): void;
        removeAllListeners(channel: string): void;
        getLinuxGsrAudioDevices(): Promise<{
          inputs: Array<{ value: string; label: string }>;
          outputs: Array<{ value: string; label: string }>;
        }>;
        getAudioSourceProperties(id: string): Promise<
          Array<{ name: string; type: string; items?: unknown[] }>
        >;
        createKillVideo(
          width: number,
          height: number,
          fps: number,
          sources: unknown[],
          audioTrackIndex: number,
        ): void;
        reconfigureBase(): void;
        toggleManualRecording(): void;
        forceStopRecording(): void;
        clipVideo(video: RendererVideo, offset: number, duration: number): void;
      };
    };
  }
}

export {};
