import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'

const host = process.env.TAURI_DEV_HOST

export default defineConfig(async () => ({
  plugins: [
    vue(),
    tailwindcss(),
  ],
  resolve: {
    // Remote desktop: every `invoke` / `listen` the UI makes goes through the
    // IPC shim (src/lib/ipc), which talks to the local backend or — in a
    // remote-desktop window — to another Devdy over the relay. The shim reaches
    // the real modules through the `@tauri-real/*` aliases. Tauri's own modules
    // import each other relatively, so they always get the real ones.
    alias: [
      { find: /^@tauri-apps\/api\/core$/, replacement: fileURLToPath(new URL('./src/lib/ipc/core.ts', import.meta.url)) },
      { find: /^@tauri-apps\/api\/event$/, replacement: fileURLToPath(new URL('./src/lib/ipc/event.ts', import.meta.url)) },
      { find: /^@tauri-real\/core$/, replacement: fileURLToPath(new URL('./node_modules/@tauri-apps/api/core.js', import.meta.url)) },
      { find: /^@tauri-real\/event$/, replacement: fileURLToPath(new URL('./node_modules/@tauri-apps/api/event.js', import.meta.url)) },
      { find: '@', replacement: fileURLToPath(new URL('./src', import.meta.url)) },
    ],
  },
  optimizeDeps: {
    // These plugins import `@tauri-apps/api/core` by name and the shim
    // intercepts some of their commands in a remote-desktop window. Pre-bundled,
    // they would get a private copy of the shim (and of its remote client,
    // never started); served as-is, the alias gives them the app's instance.
    exclude: ['@tauri-apps/plugin-dialog', '@tauri-apps/plugin-opener'],
  },
  build: {
    // Multi-page: the Tauri app (index.html) and the standalone Controller web
    // client (controller.html, U4) build into the same dist so the Controller
    // bundle can be deployed statically on the VPS next to the relay.
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        controller: fileURLToPath(new URL('./controller.html', import.meta.url)),
        mascot: fileURLToPath(new URL('./mascot.html', import.meta.url)),
        bridge: fileURLToPath(new URL('./bridge.html', import.meta.url)),
      },
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: 'ws',
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
}))
