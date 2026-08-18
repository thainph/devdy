import { createApp } from 'vue'
import { createPinia } from 'pinia'
import { VueQueryPlugin } from '@tanstack/vue-query'
import router from './router'
import { i18n } from './i18n'
import './assets/main.css'
import App from './App.vue'

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.use(i18n)
app.use(VueQueryPlugin)
app.mount('#app')
