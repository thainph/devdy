import { createApp } from 'vue'
import { createPinia } from 'pinia'
import { VueQueryPlugin } from '@tanstack/vue-query'
import router from './router'
import { i18n } from './i18n'
import './assets/main.css'
import App from './App.vue'
import { installHomeEndKeys } from './lib/homeEndKeys'
import { isRemoteMode } from './lib/ipc/remote'

installHomeEndKeys()

function mountApp() {
  const app = createApp(App)
  app.use(createPinia())
  app.use(router)
  app.use(i18n)
  app.use(VueQueryPlugin)
  app.mount('#app')
  return app
}

// The remote-desktop window (lib/remoteDesktopWindow.ts) drives another Devdy;
// its boot lives in its own chunk so the normal window never loads it.
if (isRemoteMode()) {
  import('./remoteDesktopBoot')
    .then((m) => m.bootRemoteDesktop(mountApp))
    .catch((e) => console.error('[remoteDesktop] boot failed', e))
} else {
  mountApp()
}
