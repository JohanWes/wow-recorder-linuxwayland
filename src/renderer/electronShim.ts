import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { configSchema } from 'config/configSchema';
import type { RendererVideo, VideoPlayerSettings } from 'main/types';

type Callback = (...args: unknown[]) => void;
type Config = Record<string, unknown>;

const callbacks = new Map<string, Set<Callback>>();
const subscriptions = new Map<string, Promise<UnlistenFn>>();
const pendingPayloads = new Map<string, unknown>();
const configCache: Config = Object.fromEntries(
  Object.entries(configSchema).map(([key, value]) => [key, value.default]),
);
let videoPlayerSettings: VideoPlayerSettings = { muted: false, volume: 1 };

const eventNames = [
  'updateRecStatus',
  'updateActivityStatus',
  'setDiskVideos',
  'updateDiskStatus',
  'updateMicStatus',
  'playAudio',
  'pausePlayer',
  'updateAdvancedLoggingStatus',
  'updateErrorReport',
] as const;

const warn = (operation: string, error: unknown) =>
  console.warn(`[electronShim] ${operation} failed`, error);

async function safeInvoke<T>(
  command: string,
  args?: Record<string, unknown>,
  fallback?: T,
): Promise<T> {
  try {
    return await tauriInvoke<T>(command, args);
  } catch (error) {
    warn(command, error);
    return fallback as T;
  }
}

const videoPaths = (value: unknown): string[] => {
  if (!Array.isArray(value)) return [];
  return value
    .map((item) =>
      typeof item === 'string'
        ? item
        : (item as Partial<RendererVideo>)?.videoSource,
    )
    .filter((path): path is string => typeof path === 'string');
};

const emit = (channel: string, payload: unknown) => {
  const listeners = callbacks.get(channel);
  if (!listeners) return;

  listeners.forEach((callback) => {
    if (
      channel === 'updateRecStatus' &&
      payload &&
      typeof payload === 'object'
    ) {
      const status = payload as { status?: unknown; msg?: unknown };
      callback(status.status, status.msg);
    } else {
      callback(payload);
    }
  });
};

const ensureEventSubscription = (channel: string) => {
  if (subscriptions.has(channel) || !eventNames.includes(channel as never)) {
    return;
  }

  subscriptions.set(
    channel,
    listen<unknown>(channel, ({ payload }) => emit(channel, payload)).catch(
      (error) => {
        warn(`listen(${channel})`, error);
        return () => undefined;
      },
    ),
  );
};

const invokeCommand = async (channel: string, args: unknown[] = []) => {
  switch (channel) {
    case 'selectPath':
      return safeInvoke('select_path', undefined, '');
    case 'selectFile':
    case 'selectImage':
      return safeInvoke('select_file', undefined, '');
    case 'getLinuxGsrAudioDevices':
      return safeInvoke('get_gsr_audio_devices', undefined, {
        inputs: [],
        outputs: [],
      });
    default:
      warn(`unsupported invoke(${channel})`, args);
      return undefined;
  }
};

const sendMessage = (channel: string, args: unknown[] = []) => {
  const run = async () => {
    switch (channel) {
      case 'config': {
        const [operation, keyOrValues, value] = args;
        if (operation === 'set' && typeof keyOrValues === 'string') {
          configCache[keyOrValues] = value;
          await safeInvoke('config_set', { key: keyOrValues, value });
        } else if (
          operation === 'set_values' &&
          keyOrValues &&
          typeof keyOrValues === 'object'
        ) {
          Object.assign(configCache, keyOrValues);
          await safeInvoke('config_set_values', { values: keyOrValues });
        }
        break;
      }
      case 'window': {
        const appWindow = getCurrentWindow();
        if (args[0] === 'minimize') await appWindow.minimize();
        if (args[0] === 'resize') await appWindow.toggleMaximize();
        if (args[0] === 'quit') await appWindow.close();
        break;
      }
      case 'reconfigureBase':
        await safeInvoke('reconfigure_base');
        break;
      case 'logPath':
        await safeInvoke('open_in_explorer', {
          path: String(configCache.retailLogPath ?? ''),
        });
        break;
      case 'openURL':
        await safeInvoke('open_url', { url: String(args[0] ?? '') });
        break;
      case 'writeClipboard':
        await safeInvoke('write_clipboard', { text: String(args[0] ?? '') });
        break;
      case 'test':
        await safeInvoke('test_run', {
          category: String(args[0] ?? ''),
          endTest: Boolean(args[1]),
        });
        break;
      case 'deleteVideosDisk':
        await safeInvoke('delete_videos', { videoPaths: videoPaths(args) });
        break;
      case 'videoButton':
      case 'videoButtonDisk': {
        const [operation, value, videos] = args;
        if (operation === 'open') {
          await safeInvoke('open_in_explorer', { path: String(value ?? '') });
        } else if (operation === 'protect') {
          await safeInvoke('protect_videos', {
            videoPaths: videoPaths(videos),
            protect: Boolean(value),
          });
        } else if (operation === 'tag') {
          await safeInvoke('tag_videos', {
            videoPaths: videoPaths(videos),
            tag: String(value ?? ''),
          });
        }
        break;
      }
      case 'recorder': {
        const operation = String(args[0] ?? '');
        const command =
          operation === 'linuxStartCapture'
            ? 'recorder_start'
            : operation === 'linuxStopCapture'
              ? 'recorder_stop'
              : operation === 'linuxSaveReplay'
                ? 'recorder_save_replay'
                : 'recorder_restart';
        await safeInvoke(command);
        break;
      }
      case 'videoPlayerSettings':
        if (args[0] === 'set') {
          videoPlayerSettings = args[1] as VideoPlayerSettings;
        }
        break;
      case 'deleteVideosCloud':
      case 'videoButtonCloud':
        break;
      default:
        warn(`unsupported sendMessage(${channel})`, args);
    }
  };

  void run().catch((error) => warn(`sendMessage(${channel})`, error));
};

const ipcRenderer: Window['electron']['ipcRenderer'] = {
  sendMessage,
  sendSync(channel, args) {
    if (channel === 'config' && args[0] === 'get') {
      return configCache[String(args[1])];
    }
    if (channel === 'videoPlayerSettings' && args[0] === 'get') {
      return videoPlayerSettings;
    }
    return undefined;
  },
  invoke: invokeCommand,
  on(channel, callback) {
    const listeners = callbacks.get(channel) ?? new Set<Callback>();
    listeners.add(callback);
    callbacks.set(channel, listeners);
    ensureEventSubscription(channel);
    if (pendingPayloads.has(channel)) {
      const payload = pendingPayloads.get(channel);
      pendingPayloads.delete(channel);
      queueMicrotask(() => emit(channel, payload));
    }
    return () => listeners.delete(callback);
  },
  once(channel, callback) {
    const unsubscribe = this.on(channel, (...args) => {
      unsubscribe?.();
      callback(...args);
    });
  },
  removeAllListeners(channel) {
    callbacks.delete(channel);
  },
  async getLinuxGsrAudioDevices() {
    return safeInvoke('get_gsr_audio_devices', undefined, {
      inputs: [],
      outputs: [],
    });
  },
  async getAudioSourceProperties() {
    return [];
  },
  createKillVideo() {
    warn('createKillVideo is not available in the Tauri port', undefined);
  },
  reconfigureBase() {
    sendMessage('reconfigureBase');
  },
  toggleManualRecording() {
    void safeInvoke('toggle_manual_recording');
  },
  forceStopRecording() {
    void safeInvoke('force_stop_recording');
  },
  clipVideo(video, offset, duration) {
    void safeInvoke('clip_video', {
      source: video.videoSource,
      offset,
      duration,
      metadata: video,
    });
  },
};

export async function initElectronShim(): Promise<void> {
  window.electron = { platform: 'linux', ipcRenderer };

  const loaded = await safeInvoke<Config>('config_get_all', undefined, {});
  Object.assign(configCache, loaded);

  try {
    await getCurrentWindow().onFocusChanged(({ payload }) =>
      emit('window-focus-status', payload),
    );
  } catch (error) {
    warn('window focus listener', error);
  }

  const videos = await safeInvoke<RendererVideo[]>('get_videos', undefined, []);
  pendingPayloads.set('setDiskVideos', videos);

  const version = await safeInvoke<string>('get_app_version', undefined, '');
  if (version) {
    pendingPayloads.set(
      'updateVersionDisplay',
      version.startsWith('v') ? version : `v${version}`,
    );
  }
}
