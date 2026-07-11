import { createRoot } from 'react-dom/client';
import { initElectronShim } from './electronShim';

async function render() {
  await initElectronShim();
  const { default: App } = await import('./App');
  const container = document.getElementById('root')!;
  createRoot(container).render(<App />);
}

void render();
