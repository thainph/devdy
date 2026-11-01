// Named color palettes (Settings → Color theme). Each id matches a
// `[data-theme='<id>']` block in src/assets/main.css or
// src/assets/themes/<id>.css; `default` has no attribute (the :root / .dark
// tokens). Pure data, shared by the desktop picker and the phone Controller.

export type ColorThemeGroup = 'basic' | 'macos' | 'dev' | 'seasonal'

export const COLOR_THEME_GROUPS: readonly ColorThemeGroup[] = ['basic', 'macos', 'dev', 'seasonal']

export interface ColorThemeDef {
  id: string
  group: ColorThemeGroup
  /** Follows the macOS accent color (System Settings → Appearance) when the
   *  webview exposes it; the CSS `--primary` is the fallback. */
  systemAccent?: boolean
}

export const COLOR_THEME_DEFS: readonly ColorThemeDef[] = [
  { id: 'default', group: 'basic' },
  { id: 'contrast', group: 'basic' },
  { id: 'macos', group: 'macos', systemAccent: true },
  { id: 'tahoe', group: 'macos', systemAccent: true },
  { id: 'graphite', group: 'macos' },
  { id: 'catppuccin', group: 'dev' },
  { id: 'tokyonight', group: 'dev' },
  { id: 'nord', group: 'dev' },
  { id: 'crt', group: 'dev' },
  { id: 'midautumn', group: 'seasonal' },
]

export const COLOR_THEMES: readonly string[] = COLOR_THEME_DEFS.map((d) => d.id)

/** i18n key of a palette's name, e.g. `tokyonight` → `settings.general.colorTokyonight`. */
export function colorThemeLabelKey(id: string): string {
  return `settings.general.color${id.charAt(0).toUpperCase()}${id.slice(1)}`
}

/** Whether a palette tints `--primary` with the macOS accent color. */
export function followsSystemAccent(id: string): boolean {
  return COLOR_THEME_DEFS.some((d) => d.id === id && d.systemAccent)
}

/** The palettes of one group, in display order. */
export function colorThemesIn(group: ColorThemeGroup): ColorThemeDef[] {
  return COLOR_THEME_DEFS.filter((d) => d.group === group)
}
