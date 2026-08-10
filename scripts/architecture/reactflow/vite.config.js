import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// base: './' keeps built asset paths relative so the dist works when served
// from any static host (or previewed) without a fixed mount path.
export default defineConfig({
  plugins: [react()],
  base: './',
});
