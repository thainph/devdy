// WCAG contrast checks for the color themes in src/assets/main.css and
// src/assets/themes/*.css. Pure logic, no DOM: the CSS is passed in as text
// (see __tests__/themeContrast.test.mjs, which runs it over every theme).

export type Rgb = [number, number, number]
export type Tokens = Record<string, string>
export type Mode = 'light' | 'dark'

export interface ThemeBlocks {
  root: Tokens
  dark: Tokens
  /** theme id → tokens of `[data-theme='id']:not(.dark)` / `[data-theme='id'].dark`. */
  themes: Record<string, { light?: Tokens; dark?: Tokens }>
}

export interface ContrastFailure {
  pair: string
  ratio: number
  min: number
}

/** [foreground token, background token, minimum ratio]. */
export const CONTRAST_PAIRS: [string, string, number][] = [
  ['foreground', 'background', 4.5],
  ['card-foreground', 'card', 4.5],
  ['popover-foreground', 'popover', 4.5],
  ['foreground', 'sidebar', 4.5],
  ['muted-foreground', 'background', 4.5],
  ['muted-foreground', 'muted', 4.5],
  ['primary-foreground', 'primary', 4.5],
  ['destructive-foreground', 'destructive', 4.5],
  ['secondary-foreground', 'secondary', 4.5],
  ['accent-foreground', 'accent', 4.5],
  ['success-foreground', 'success', 4.5],
  ['warning-foreground', 'warning', 4.5],
  ['info-foreground', 'info', 4.5],
]

/** Tokens every named-theme block must define itself (the rest may fall back to base). */
export const REQUIRED_TOKENS = [
  'background',
  'foreground',
  'card',
  'card-foreground',
  'popover',
  'popover-foreground',
  'primary',
  'primary-foreground',
  'secondary',
  'secondary-foreground',
  'muted',
  'muted-foreground',
  'accent',
  'accent-foreground',
  'destructive',
  'destructive-foreground',
  'border',
  'input',
  'ring',
  'sidebar',
]

/** Additionally required in the standalone theme files (src/assets/themes/*.css). */
export const AURORA_TOKENS = ['aurora-1', 'aurora-2']

/** Bare HSL triplet `"243 76% 59%"` (optionally `/ alpha`, ignored) → sRGB 0–255. */
export function parseHsl(value: string): Rgb {
  const m = value
    .trim()
    .match(/^(-?[\d.]+)(?:deg)?[\s,]+([\d.]+)%[\s,]+([\d.]+)%(?:\s*\/\s*[\d.]+%?)?$/)
  if (!m) throw new Error(`not an HSL triplet: "${value}"`)
  const h = (((Number(m[1]) % 360) + 360) % 360) / 360
  const s = Number(m[2]) / 100
  const l = Number(m[3]) / 100
  if (s === 0) {
    const v = Math.round(l * 255)
    return [v, v, v]
  }
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s
  const p = 2 * l - q
  const hue = (t: number) => {
    if (t < 0) t += 1
    if (t > 1) t -= 1
    if (t < 1 / 6) return p + (q - p) * 6 * t
    if (t < 1 / 2) return q
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6
    return p
  }
  return [hue(h + 1 / 3), hue(h), hue(h - 1 / 3)].map((c) => Math.round(c * 255)) as Rgb
}

/** WCAG 2.x relative luminance of an sRGB color. */
export function relativeLuminance([r, g, b]: Rgb): number {
  const lin = (c: number) => {
    const v = c / 255
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4
  }
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

export function contrastRatio(a: Rgb, b: Rgb): number {
  const la = relativeLuminance(a)
  const lb = relativeLuminance(b)
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05)
}

function parseDeclarations(body: string): Tokens {
  const out: Tokens = {}
  for (const decl of body.split(';')) {
    const m = decl.match(/^\s*--([\w-]+)\s*:\s*([\s\S]*?)\s*$/)
    if (m && m[2]) out[m[1]] = m[2]
  }
  return out
}

/**
 * Collect the token maps of `:root`, `.dark`, `[data-theme='x']:not(.dark)` and
 * `[data-theme='x'].dark` blocks (also inside `@layer`); every other rule is
 * ignored. Repeated selectors merge in source order.
 */
export function parseThemeBlocks(css: string): ThemeBlocks {
  const blocks: ThemeBlocks = { root: {}, dark: {}, themes: {} }
  const src = css.replace(/\/\*[\s\S]*?\*\//g, '')
  // Innermost rules only: a selector run followed by a brace-free body.
  const rule = /([^{}]+)\{([^{}]*)\}/g
  let m: RegExpExecArray | null
  while ((m = rule.exec(src))) {
    const tokens = parseDeclarations(m[2])
    for (const raw of m[1].split(',')) {
      const sel = raw.trim().replace(/\s+/g, ' ')
      if (sel === ':root') Object.assign(blocks.root, tokens)
      else if (sel === '.dark') Object.assign(blocks.dark, tokens)
      else {
        const t = sel.match(/^\[data-theme=(['"]?)([\w-]+)\1\](:not\(\.dark\)|\.dark)$/)
        if (!t) continue
        const entry = (blocks.themes[t[2]] ??= {})
        const mode: Mode = t[3] === '.dark' ? 'dark' : 'light'
        entry[mode] = { ...entry[mode], ...tokens }
      }
    }
  }
  return blocks
}

/** Merge several parsed files (e.g. main.css + every themes/*.css) in order. */
export function mergeThemeBlocks(list: ThemeBlocks[]): ThemeBlocks {
  const out: ThemeBlocks = { root: {}, dark: {}, themes: {} }
  for (const b of list) {
    Object.assign(out.root, b.root)
    Object.assign(out.dark, b.dark)
    for (const [id, t] of Object.entries(b.themes)) {
      const e = (out.themes[id] ??= {})
      if (t.light) e.light = { ...e.light, ...t.light }
      if (t.dark) e.dark = { ...e.dark, ...t.dark }
    }
  }
  return out
}

/**
 * Effective tokens of theme `id` in `mode`: base (`:root`, plus `.dark` in dark
 * mode) with the theme's own block on top. `default` (or an unknown id) is the
 * base alone.
 */
export function resolveTheme(blocks: ThemeBlocks, id: string, mode: Mode): Tokens {
  const base = mode === 'dark' ? { ...blocks.root, ...blocks.dark } : { ...blocks.root }
  return { ...base, ...(blocks.themes[id]?.[mode] ?? {}) }
}

/** Pairs below their minimum ratio; a pair with a missing token is a failure (ratio 0). */
export function checkTheme(tokens: Tokens): ContrastFailure[] {
  const failures: ContrastFailure[] = []
  for (const [fg, bg, min] of CONTRAST_PAIRS) {
    const pair = `${fg}/${bg}`
    if (!tokens[fg] || !tokens[bg]) {
      failures.push({ pair, ratio: 0, min })
      continue
    }
    const ratio = contrastRatio(parseHsl(tokens[fg]), parseHsl(tokens[bg]))
    if (ratio < min) failures.push({ pair, ratio: Math.floor(ratio * 100) / 100, min })
  }
  return failures
}

/** Required tokens the block does not define itself. */
export function missingTokens(block: Tokens | undefined, required: string[]): string[] {
  return required.filter((k) => !block?.[k])
}
