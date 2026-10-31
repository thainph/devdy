// Appearance of the phone Controller: the Host pushes the desktop's theme +
// color palette, and this phone may override either one locally. `host` means
// "follow the desktop" for that field. Pure logic, no DOM (see
// src/controller/appearance.ts for applying it).

export const FOLLOW_HOST = 'host'

export const THEME_MODES = ['light', 'dark', 'system'] as const

/** Palettes defined in main.css (`default` = no `data-theme` attribute). */
export const COLOR_THEMES = [
  'default',
  'ocean',
  'forest',
  'sunset',
  'rose',
  'teal',
  'lagoon',
  'mint',
  'midautumn',
] as const

export interface Appearance {
  /** `light` / `dark` / `system`. */
  theme: string
  /** Named palette (`default` = no `data-theme` attribute, see main.css). */
  color_theme: string
}

/** Same fields as {@link Appearance}, each also allowing {@link FOLLOW_HOST}. */
export type AppearanceOverride = Appearance

export const NO_OVERRIDE: AppearanceOverride = { theme: FOLLOW_HOST, color_theme: FOLLOW_HOST }

/** The look used before any Host has been seen (matches the desktop defaults). */
export const DEFAULT_APPEARANCE: Appearance = { theme: 'system', color_theme: 'default' }

function pick(value: unknown, allowed: readonly string[]): string {
  return typeof value === 'string' && allowed.includes(value) ? value : FOLLOW_HOST
}

/** Read a stored override; unknown or missing fields fall back to the Host. */
export function parseOverride(raw: unknown): AppearanceOverride {
  if (!raw || typeof raw !== 'object') return { ...NO_OVERRIDE }
  const o = raw as Record<string, unknown>
  return {
    theme: pick(o.theme, THEME_MODES),
    color_theme: pick(o.color_theme, COLOR_THEMES),
  }
}

/** The look to show: each overridden field wins over the Host's. */
export function resolveAppearance(host: Appearance | null, override: AppearanceOverride): Appearance {
  const base = host ?? DEFAULT_APPEARANCE
  return {
    theme: override.theme !== FOLLOW_HOST ? override.theme : base.theme,
    color_theme: override.color_theme !== FOLLOW_HOST ? override.color_theme : base.color_theme,
  }
}
