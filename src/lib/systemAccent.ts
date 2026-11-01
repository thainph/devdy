// macOS accent color (System Settings → Appearance → Accent color) for the
// palettes flagged `systemAccent` in colorThemes.ts. WebKit exposes it as the
// `-apple-system-control-accent` CSS color (other engines: `AccentColor`); we
// resolve it on a probe element and write `--primary` / `--ring` /
// `--primary-foreground` as inline vars on <html>, which beat the theme blocks.
// Engines without either keyword keep the theme's own `--primary`.

import { followsSystemAccent } from './colorThemes.ts'

const KEYWORDS = ['-apple-system-control-accent', 'AccentColor']
const VARS = ['--primary', '--ring', '--primary-foreground'] as const

/** `rgb(10, 132, 255)` / `rgba(…)` → `[r, g, b]` (0–255), or null. */
export function parseRgb(css: string): [number, number, number] | null {
  const m = css.match(/rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)/)
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null
}

/** RGB → bare HSL channels `H S% L%` (the token format in main.css). */
export function rgbToHslChannels([r, g, b]: [number, number, number]): string {
  const rn = r / 255
  const gn = g / 255
  const bn = b / 255
  const max = Math.max(rn, gn, bn)
  const min = Math.min(rn, gn, bn)
  const l = (max + min) / 2
  let h = 0
  let s = 0
  if (max !== min) {
    const d = max - min
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min)
    if (max === rn) h = (gn - bn) / d + (gn < bn ? 6 : 0)
    else if (max === gn) h = (bn - rn) / d + 2
    else h = (rn - gn) / d + 4
    h *= 60
  }
  return `${Math.round(h)} ${Math.round(s * 100)}% ${Math.round(l * 100)}%`
}

function luminance([r, g, b]: [number, number, number]): number {
  const lin = (c: number) => {
    const v = c / 255
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4
  }
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/** Text on an accent fill: white like native push buttons (macOS blue is only
 *  ~4:1, orange ~2.2:1), dark only for the light accents (yellow). */
export function accentForeground(rgb: [number, number, number]): string {
  return 1.05 / (luminance(rgb) + 0.05) >= 2 ? '0 0% 100%' : '0 0% 10%'
}

function readAccent(): [number, number, number] | null {
  const keyword = KEYWORDS.find((k) => CSS.supports('color', k))
  if (!keyword) return null
  const probe = document.createElement('span')
  probe.style.color = keyword
  probe.style.display = 'none'
  document.body.appendChild(probe)
  const rgb = parseRgb(getComputedStyle(probe).color)
  probe.remove()
  return rgb
}

/** Apply (or clear) the macOS accent for the given palette id. */
export function applySystemAccent(theme: string): void {
  const style = document.documentElement.style
  const rgb = followsSystemAccent(theme) ? readAccent() : null
  if (!rgb) {
    for (const v of VARS) style.removeProperty(v)
    return
  }
  const hsl = rgbToHslChannels(rgb)
  style.setProperty('--primary', hsl)
  style.setProperty('--ring', hsl)
  style.setProperty('--primary-foreground', accentForeground(rgb))
}
