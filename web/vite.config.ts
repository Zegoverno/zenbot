/// <reference types="vitest/config" />
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  build: { target: 'es2022', sourcemap: false },
  // The dev server pre-bundles what every page may import, the lazily loaded writer included, so a
  // cold cache doesn't reload the page halfway (the e2e tests run on a fresh install).
  optimizeDeps: { entries: ['index.html', 'src/**/*.svelte', 'src/**/*.ts', '!src/**/*.test.ts'] },
  test: { include: ['src/**/*.test.ts', 'server/**/*.test.ts'] },
});
