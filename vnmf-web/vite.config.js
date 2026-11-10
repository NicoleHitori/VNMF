// SPDX-License-Identifier: GPL-3.0-or-later
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// The build goes into vnmf-server/web, which the server embeds into its binary (see vnmf-server/build.rs).
export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: '../vnmf-server/web',
    emptyOutDir: true,
    assetsDir: 'assets',
    target: 'es2022',
    sourcemap: false,
  },
  // `npm run dev`: the interface with hot reload, the API from a running vnmf-server.
  server: {
    proxy: { '/api': 'http://127.0.0.1:8470' },
  },
});
