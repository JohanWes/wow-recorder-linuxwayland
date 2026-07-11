import { invoke } from '@tauri-apps/api/core';

export const getMediaUrl = (source: string): Promise<string> => {
  if (source.startsWith('https://') || source.startsWith('http://')) {
    return Promise.resolve(source);
  }
  return invoke<string>('get_video_url', { path: source });
};
