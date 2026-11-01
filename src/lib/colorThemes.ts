// Named color palettes (Settings → Color theme). Each id matches a
// `[data-theme='<id>']` block in src/assets/main.css or
// src/assets/themes/<id>.css; `default` has no attribute (the :root / .dark
// tokens). Pure data, shared by the desktop picker and the phone Controller.

export type ColorThemeGroup = 'classic' | 'editor' | 'trend' | 'seasonal'

export const COLOR_THEME_GROUPS: readonly ColorThemeGroup[] = ['classic', 'editor', 'trend', 'seasonal']

export interface ColorThemeDef {
  id: string
  group: ColorThemeGroup
}

export const COLOR_THEME_DEFS: readonly ColorThemeDef[] = [
  { id: 'default', group: 'classic' },
  { id: 'ocean', group: 'classic' },
  { id: 'forest', group: 'classic' },
  { id: 'sunset', group: 'classic' },
  { id: 'rose', group: 'classic' },
  { id: 'teal', group: 'classic' },
  { id: 'lagoon', group: 'classic' },
  { id: 'mint', group: 'classic' },
  { id: 'catppuccin', group: 'editor' },
  { id: 'tokyonight', group: 'editor' },
  { id: 'rosepine', group: 'editor' },
  { id: 'nord', group: 'editor' },
  { id: 'dracula', group: 'editor' },
  { id: 'gruvbox', group: 'editor' },
  { id: 'kanagawa', group: 'editor' },
  { id: 'neon', group: 'trend' },
  { id: 'sand', group: 'trend' },
  { id: 'contrast', group: 'trend' },
  { id: 'midautumn', group: 'seasonal' },
  { id: 'christmas', group: 'seasonal' },
  { id: 'tet', group: 'seasonal' },
]

export const COLOR_THEMES: readonly string[] = COLOR_THEME_DEFS.map((d) => d.id)

/** i18n key of a palette's name, e.g. `rosepine` → `settings.general.colorRosepine`. */
export function colorThemeLabelKey(id: string): string {
  return `settings.general.color${id.charAt(0).toUpperCase()}${id.slice(1)}`
}

/** The palettes of one group, in display order. */
export function colorThemesIn(group: ColorThemeGroup): ColorThemeDef[] {
  return COLOR_THEME_DEFS.filter((d) => d.group === group)
}
