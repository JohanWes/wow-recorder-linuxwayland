import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'node:path';

const src = path.resolve(__dirname, 'src');

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      main: path.join(src, 'main'), config: path.join(src, 'config'),
      localisation: path.join(src, 'localisation'), types: path.join(src, 'types'),
      renderer: path.join(src, 'renderer'), parsing: path.join(src, 'parsing'),
    },
  },
});
