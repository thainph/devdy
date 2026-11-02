// Appearance of the phone Controller: light / dark / system, following the
// desktop's theme unless this phone overrides it locally. The phone always
// uses the macOS Tahoe skin — the Host's `color_theme` is ignored. Pure
// logic, no DOM (see src/controller/appearance.ts for applying it).

export const FOLLOW_HOST = 'host'

export const THEME_MODES = ['light', 'dark', 'system'] as const

/** The theme mode to show: `light` / `dark` / `system`. */
export interface Appearance {
  theme: string
}

/** Same field as {@link Appearance}, also allowing {@link FOLLOW_HOST}. */
export type AppearanceOverride = Appearance

export const NO_OVERRIDE: AppearanceOverride = { theme: FOLLOW_HOST }

/** The look used before any Host has been seen (matches the desktop default). */
export const DEFAULT_APPEARANCE: Appearance = { theme: 'system' }

function isMode(value: unknown): value is string {
  return typeof value === 'string' && (THEME_MODES as readonly string[]).includes(value)
}

/** Read a Host frame or a stored Host appearance; an unknown mode means `system`. */
export function parseAppearance(raw: unknown): Appearance | null {
  if (!raw || typeof raw !== 'object') return null
  const theme = (raw as Record<string, unknown>).theme
  return { theme: isMode(theme) ? theme : DEFAULT_APPEARANCE.theme }
}

/** Read a stored override; an unknown or missing mode falls back to the Host. */
export function parseOverride(raw: unknown): AppearanceOverride {
  if (!raw || typeof raw !== 'object') return { ...NO_OVERRIDE }
  const theme = (raw as Record<string, unknown>).theme
  return { theme: isMode(theme) ? theme : FOLLOW_HOST }
}

/** The look to show: an override wins over the Host's mode. */
export function resolveAppearance(host: Appearance | null, override: AppearanceOverride): Appearance {
  return { theme: override.theme !== FOLLOW_HOST ? override.theme : (host ?? DEFAULT_APPEARANCE).theme }
}
