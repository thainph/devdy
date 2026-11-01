// Unit tests for lib/themeContrast.ts + a WCAG contrast sweep over every color
// theme in src/assets/main.css and src/assets/themes/*.css.
// Run: node --test src/lib/__tests__/themeContrast.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, readdirSync, existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  AURORA_TOKENS,
  REQUIRED_TOKENS,
  checkTheme,
  contrastRatio,
  mergeThemeBlocks,
  missingTokens,
  parseHsl,
  parseThemeBlocks,
  relativeLuminance,
  resolveTheme,
} from '../themeContrast.ts'

const close = (a, b, eps = 0.01) => assert.ok(Math.abs(a - b) < eps, `${a} ≉ ${b}`)

test('parseHsl converts bare triplets', () => {
  assert.deepEqual(parseHsl('0 0% 100%'), [255, 255, 255])
  assert.deepEqual(parseHsl('0 0% 0%'), [0, 0, 0])
  assert.deepEqual(parseHsl('0 100% 50%'), [255, 0, 0])
  assert.deepEqual(parseHsl('120 100% 50%'), [0, 255, 0])
  assert.deepEqual(parseHsl('240 100% 50%'), [0, 0, 255])
  assert.deepEqual(parseHsl('243 76% 59%'), [79, 71, 230]) // ≈ indigo-600 #4f46e5
  assert.deepEqual(parseHsl(' 0 0% 50% / 0.4 '), [128, 128, 128])
  assert.throws(() => parseHsl('#fff'))
})

test('luminance and contrast ratio match WCAG reference values', () => {
  close(relativeLuminance([255, 255, 255]), 1)
  close(relativeLuminance([0, 0, 0]), 0)
  close(contrastRatio([255, 255, 255], [0, 0, 0]), 21)
  close(contrastRatio([0, 0, 0], [255, 255, 255]), 21)
  close(contrastRatio([255, 0, 0], [255, 255, 255]), 4.0)
  close(contrastRatio([0, 0, 255], [255, 255, 255]), 8.59)
  close(contrastRatio([118, 118, 118], [255, 255, 255]), 4.54) // #767676, the AA grey
  close(contrastRatio([119, 119, 119], [119, 119, 119]), 1)
})

test('parseThemeBlocks picks only theme selectors and tolerates comments', () => {
  const css = `
    /* :root { --background: 0 0% 0%; } */
    @layer base {
      :root { --background: 0 0% 100%; --success: 161 94% 30%; /* emerald */ --radius: 1rem; }
      .dark { --background: 0 0% 0%; }
      [data-theme='x']:not(.dark) { --background: 10 10% 90%; }
      [data-theme="x"].dark { --background: 10 10% 10%; }
      .dark .card { --background: 1 1% 1%; }
      body::before { color: red; }
    }`
  const b = parseThemeBlocks(css)
  assert.deepEqual(b.root, { background: '0 0% 100%', success: '161 94% 30%', radius: '1rem' })
  assert.deepEqual(b.dark, { background: '0 0% 0%' })
  assert.deepEqual(b.themes, {
    x: { light: { background: '10 10% 90%' }, dark: { background: '10 10% 10%' } },
  })
  assert.deepEqual(resolveTheme(b, 'x', 'dark'), {
    background: '10 10% 10%',
    success: '161 94% 30%',
    radius: '1rem',
  })
  assert.equal(resolveTheme(b, 'default', 'light').background, '0 0% 100%')
})

test('checkTheme reports failing and missing pairs', () => {
  const f = checkTheme({ foreground: '0 0% 60%', background: '0 0% 100%' })
  const fg = f.find((x) => x.pair === 'foreground/background')
  assert.ok(fg && fg.ratio < 4.5 && fg.min === 4.5)
  assert.ok(f.some((x) => x.pair === 'info-foreground/info' && x.ratio === 0))
})

// --- Sweep over the real theme files -------------------------------------------

const assets = join(dirname(fileURLToPath(import.meta.url)), '../../assets')
const mainBlocks = parseThemeBlocks(readFileSync(join(assets, 'main.css'), 'utf8'))
const themesDir = join(assets, 'themes')
const themeFiles = existsSync(themesDir)
  ? readdirSync(themesDir)
      .filter((f) => f.endsWith('.css'))
      .sort()
      .map((f) => ({ file: f, blocks: parseThemeBlocks(readFileSync(join(themesDir, f), 'utf8')) }))
  : []
const all = mergeThemeBlocks([mainBlocks, ...themeFiles.map((t) => t.blocks)])
const themeIds = ['default', ...Object.keys(all.themes)]

test('base blocks and at least the legacy themes are discovered', () => {
  assert.ok(Object.keys(mainBlocks.root).length > 0)
  assert.ok(Object.keys(mainBlocks.dark).length > 0)
  for (const id of ['midautumn']) {
    assert.ok(all.themes[id], `theme ${id} not found`)
  }
})

test('base :root / .dark define every token the pairs need', () => {
  for (const mode of ['light', 'dark']) {
    const base = resolveTheme(all, 'default', mode)
    const need = REQUIRED_TOKENS.concat('success', 'success-foreground', 'warning')
      .concat('warning-foreground', 'info', 'info-foreground')
    assert.deepEqual(missingTokens(base, need), [], `base ${mode}`)
  }
})

for (const [id, t] of Object.entries(mainBlocks.themes)) {
  test(`main.css theme ${id} defines the required tokens`, () => {
    for (const mode of ['light', 'dark']) {
      assert.deepEqual(missingTokens(t[mode], REQUIRED_TOKENS), [], `${id} ${mode}`)
    }
  })
}

for (const { file, blocks } of themeFiles) {
  test(`themes/${file} defines the required tokens`, () => {
    const ids = Object.keys(blocks.themes)
    assert.ok(ids.length > 0, `${file} has no [data-theme] blocks`)
    for (const id of ids) {
      for (const mode of ['light', 'dark']) {
        const missing = missingTokens(blocks.themes[id][mode], REQUIRED_TOKENS.concat(AURORA_TOKENS))
        assert.deepEqual(missing, [], `${id} ${mode}`)
      }
    }
  })
}

for (const id of themeIds) {
  for (const mode of ['light', 'dark']) {
    test(`contrast: ${id} ${mode}`, () => {
      const failures = checkTheme(resolveTheme(all, id, mode))
      assert.deepEqual(
        failures,
        [],
        failures.map((f) => `${id} ${mode} ${f.pair} ${f.ratio} < ${f.min}`).join('\n'),
      )
    })
  }
}
