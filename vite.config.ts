import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import VueI18nPlugin from '@intlify/unplugin-vue-i18n/vite'
import { fileURLToPath, URL } from 'node:url'

const host = process.env.TAURI_DEV_HOST

export default defineConfig(async () => ({
  plugins: [
    vue(),
    tailwindcss(),
    // Precompile the locale JSON at build time, so the bundles ship vue-i18n's
    // runtime-only build (no message compiler) and no message is parsed on the
    // device. `pnpm check:i18n` / the build now fail on a malformed message.
    VueI18nPlugin({
      include: [fileURLToPath(new URL('./src/i18n/locales/**', import.meta.url))],
      strictMessage: false,
      dropMessageCompiler: true,
    }),
  ],
  resolve: {
    alias: [
      { find: '@', replacement: fileURLToPath(new URL('./src', import.meta.url)) },
    ],
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
