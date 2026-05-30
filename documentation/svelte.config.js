import adapter from '@sveltejs/adapter-static';
import { specraConfig } from 'specra/svelte-config';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

const config = specraConfig({
  vitePreprocess: { vitePreprocess },
  kit: {
    adapter: adapter(),
    prerender: { handleHttpError: 'warn', handleMissingId: 'warn', handleUnseenRoutes: 'warn' }
  }
});

export default config;
