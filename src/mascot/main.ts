// Slim entry point for the DY Cyber Fox "desktop pet" window.
//
// The pet used to load the FULL app (index.html?mascotWindow=1) just to draw one
// fox — booting Pinia, the router, vue-query, every store and pop-out view inside
// a second transparent always-on-top webview. This entry mounts ONLY MascotWindow
// (Vue + i18n + the canvas renderer), so the pet window stays a lightweight, dumb
// renderer fed by events from the main window.
import { createApp } from 'vue'
import '@/assets/main.css'
import { i18n } from '@/i18n'
import MascotWindow from '@/views/MascotWindow.vue'

const app = createApp(MascotWindow)
app.use(i18n)
app.mount('#app')
