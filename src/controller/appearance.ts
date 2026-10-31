// Mirror the desktop's look (Settings → theme + color palette) in the
// Controller. The Host pushes an `appearance` frame on pairing; the last one is
// cached so the next load (and the link / OTP screens before pairing) starts in
// the same colors instead of flashing the default palette.
//
// This phone can override the theme and / or palette (the theme sheet); an
// override is kept on this device only and wins over the Host's field.

import { readonly, ref } from 'vue'
import {
  NO_OVERRIDE,
  parseOverride,
  resolveAppearance,
  type Appearance,
  type AppearanceOverride,
} from '@/lib/controllerAppearance'

export type { Appearance, AppearanceOverride }

const STORAGE_KEY = 'devdy.controller.appearance'
const OVERRIDE_KEY = 'devdy.controller.appearanceOverride'

const darkQuery = window.matchMedia('(prefers-color-scheme: dark)')
let followSystem = false

/** The Host's last appearance (null until one is known). */
const host = ref<Appearance | null>(null)
const override = ref<AppearanceOverride>({ ...NO_OVERRIDE })

/** The Host's look and this phone's choice, for the theme sheet. */
export const hostAppearance = readonly(host)
export const appearanceOverride = readonly(override)

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

function applyAppearance(a: Appearance): void {
  const palette = a.color_theme && a.color_theme !== 'default' ? a.color_theme : ''
  if (palette) document.documentElement.setAttribute('data-theme', palette)
  else document.documentElement.removeAttribute('data-theme')

  followSystem = a.theme !== 'dark' && a.theme !== 'light'
  setDark(followSystem ? darkQuery.matches : a.theme === 'dark')
}

function applyCurrent(): void {
  applyAppearance(resolveAppearance(host.value, override.value))
}

function store(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value))
  } catch {
    // Private mode / storage full: the look still applies for this load.
  }
}

function load(key: string): unknown {
  try {
    const raw = localStorage.getItem(key)
    return raw ? JSON.parse(raw) : null
  } catch {
    // A corrupt entry just means nothing stored.
    return null
  }
}

/** Apply and remember the Host's appearance (an override still wins). */
export function setAppearance(a: Appearance): void {
  host.value = { theme: a.theme, color_theme: a.color_theme }
  applyCurrent()
  store(STORAGE_KEY, host.value)
}

/** Apply and remember this phone's own theme / palette choice. */
export function setAppearanceOverride(o: AppearanceOverride): void {
  override.value = parseOverride(o)
  applyCurrent()
  store(OVERRIDE_KEY, override.value)
}

/** Apply the last Host appearance and this phone's override (call before mounting). */
export function restoreAppearance(): void {
  const a = load(STORAGE_KEY) as Partial<Appearance> | null
  if (a && typeof a.theme === 'string' && typeof a.color_theme === 'string') {
    host.value = { theme: a.theme, color_theme: a.color_theme }
  }
  override.value = parseOverride(load(OVERRIDE_KEY))
  applyCurrent()
}
