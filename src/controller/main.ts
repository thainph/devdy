// Entry point for the standalone Controller web client (U4). This bundle is
// deployed statically on the VPS next to the relay and runs in a plain browser
// (phone / other PC) — it MUST NOT import any `@tauri-apps/*` API.
import { createApp } from 'vue'
import '@/assets/main.css'
import App from './App.vue'

// Framework-independent error surface (requirement: never fail silently). If
// anything throws — including a failed dynamic import / chunk 404, or a bug that
// crashes Vue — show a persistent banner instead of a blank/frozen screen.
function showFatal(msg: string): void {
  let el = document.getElementById('fatal-error')
  if (!el) {
    el = document.createElement('div')
    el.id = 'fatal-error'
    el.style.cssText =
      'position:fixed;left:0;right:0;bottom:0;z-index:99999;background:#7f1d1d;color:#fff;' +
      'font:12px/1.4 system-ui,-apple-system,sans-serif;padding:8px 12px;text-align:center'
    document.body.appendChild(el)
  }
  el.textContent = msg
}

window.addEventListener('error', (e) => {
  showFatal(`Lỗi: ${e.message || 'unknown'} — thử tải lại trang.`)
})
window.addEventListener('unhandledrejection', (e) => {
  const reason = e.reason as { message?: string } | string | undefined
  const msg = (typeof reason === 'string' ? reason : reason?.message) || 'unknown'
  showFatal(`Lỗi tải tài nguyên: ${msg} — thử tải lại trang.`)
})

const app = createApp(App)
app.config.errorHandler = (err) => {
  const msg = err instanceof Error ? err.message : String(err)
  showFatal(`Lỗi ứng dụng: ${msg}`)
  console.error(err)
}
app.mount('#app')
