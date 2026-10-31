// Mirror the desktop's look (Settings → theme + color palette) in the
// Controller. The Host pushes an `appearance` frame on pairing; the last one is
// cached so the next load (and the link / OTP screens before pairing) starts in
// the same colors instead of flashing the default palette.

const STORAGE_KEY = 'devdy.controller.appearance'

export interface Appearance {
  /** `light` / `dark` / `system`. */
  theme: string
  /** Named palette (`default` = no `data-theme` attribute, see main.css). */
  color_theme: string
}

const darkQuery = window.matchMedia('(prefers-color-scheme: dark)')
let followSystem = false

function setDark(dark: boolean): void {
  const root = document.documentElement
  root.classList.toggle('dark', dark)
  // Status-bar / browser chrome color follows the page background (main.css
  // stores it as bare `H S% L%` channels).
  const bg = getComputedStyle(root).getPropertyValue('--background').trim()
  const meta = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]')
  if (meta && bg) meta.content = `hsl(${bg})`
}

darkQuery.addEventListener('change', (e) => {
  if (followSystem) setDark(e.matches)
})

export function applyAppearance(a: Appearance): void {
  const palette = a.color_theme && a.color_theme !== 'default' ? a.color_theme : ''
  if (palette) document.documentElement.setAttribute('data-theme', palette)
  else document.documentElement.removeAttribute('data-theme')

  followSystem = a.theme !== 'dark' && a.theme !== 'light'
  setDark(followSystem ? darkQuery.matches : a.theme === 'dark')
}

/** Apply and remember the Host's appearance. */
export function setAppearance(a: Appearance): void {
  applyAppearance(a)
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(a))
  } catch {
    // Private mode / storage full: the look still applies for this load.
  }
}

/** Apply the last appearance the Host sent, if any (call before mounting). */
export function restoreAppearance(): void {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return
    const a = JSON.parse(raw) as Partial<Appearance>
    if (typeof a.theme === 'string' && typeof a.color_theme === 'string') {
      applyAppearance({ theme: a.theme, color_theme: a.color_theme })
    }
  } catch {
    // A corrupt entry just means the default look.
  }
}
