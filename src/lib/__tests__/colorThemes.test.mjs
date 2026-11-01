// Unit tests for lib/colorThemes.ts (palette registry) — every registered id
// has CSS and an i18n name in both locales.
// Run: node --test src/lib/__tests__/colorThemes.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { COLOR_THEME_DEFS, COLOR_THEME_GROUPS, COLOR_THEMES, colorThemeLabelKey, colorThemesIn } from '../colorThemes.ts'

const src = fileURLToPath(new URL('../../', import.meta.url))
const mainCss = readFileSync(`${src}assets/main.css`, 'utf8')

test('ids are unique and default comes first', () => {
  assert.equal(new Set(COLOR_THEMES).size, COLOR_THEMES.length)
  assert.equal(COLOR_THEMES[0], 'default')
})

test('every group is used and every theme is in a known group', () => {
  for (const g of COLOR_THEME_GROUPS) assert.ok(colorThemesIn(g).length > 0, g)
  for (const d of COLOR_THEME_DEFS) assert.ok(COLOR_THEME_GROUPS.includes(d.group), d.id)
})

test('every named theme has light and dark CSS blocks', () => {
  for (const id of COLOR_THEMES.filter((t) => t !== 'default')) {
    const file = `${src}assets/themes/${id}.css`
    const css = existsSync(file) ? readFileSync(file, 'utf8') : mainCss
    assert.ok(css.includes(`[data-theme='${id}']:not(.dark)`), `${id} light`)
    assert.ok(css.includes(`[data-theme='${id}'].dark`), `${id} dark`)
    if (existsSync(file)) assert.ok(mainCss.includes(`@import "./themes/${id}.css";`), `${id} imported`)
  }
})

test('every theme and group has a name in en and vi', () => {
  for (const loc of ['en', 'vi']) {
    const general = JSON.parse(readFileSync(`${src}i18n/locales/${loc}/settings.json`, 'utf8')).general
    for (const id of COLOR_THEMES) {
      const key = colorThemeLabelKey(id).replace('settings.general.', '')
      assert.equal(typeof general[key], 'string', `${loc} ${key}`)
    }
    for (const g of COLOR_THEME_GROUPS) assert.equal(typeof general.colorGroup[g], 'string', `${loc} ${g}`)
  }
})

test('label key capitalizes the id', () => {
  assert.equal(colorThemeLabelKey('rosepine'), 'settings.general.colorRosepine')
})
