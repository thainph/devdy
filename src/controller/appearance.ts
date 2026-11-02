// Mirror the desktop's light / dark / system choice in the Controller. The Host
// pushes an `appearance` frame on pairing; the last one is cached so the next
// load (and the link / OTP screens before pairing) starts in the same mode
// instead of flashing the default one. The phone always wears the macOS Tahoe
// skin (`data-theme` is fixed in controller.html) — the desktop's color theme
// is not mirrored.
//
// This phone can override the mode (the theme sheet); an override is kept on
// this device only and wins over the Host's.

import { readonly, ref } from 'vue'
import {
  NO_OVERRIDE,
  parseAppearance,
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

function applyCurrent(): void {
  const { theme } = resolveAppearance(host.value, override.value)
  followSystem = theme !== 'dark' && theme !== 'light'
  setDark(followSystem ? darkQuery.matches : theme === 'dark')
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

/** Apply and remember the Host's mode (an override still wins). */
export function setAppearance(a: Appearance): void {
  host.value = parseAppearance(a)
  applyCurrent()
  store(STORAGE_KEY, host.value)
}

/** Apply and remember this phone's own mode. */
export function setAppearanceOverride(o: AppearanceOverride): void {
  override.value = parseOverride(o)
  applyCurrent()
  store(OVERRIDE_KEY, override.value)
}

/** Apply the last Host mode and this phone's override (call before mounting). */
export function restoreAppearance(): void {
  host.value = parseAppearance(load(STORAGE_KEY))
  override.value = parseOverride(load(OVERRIDE_KEY))
  applyCurrent()
}
