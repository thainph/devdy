// Unit tests for lib/paletteSearch.ts (command palette matching + ranking).
// Run: node --test src/lib/__tests__/paletteSearch.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { foldText, matchScore, rankItems } from '../paletteSearch.ts'

test('foldText strips Vietnamese diacritics and folds đ', () => {
  assert.equal(foldText('Lịch'), 'lich')
  assert.equal(foldText('Đồ án Tiếng Việt'), 'do an tieng viet')
  assert.equal(foldText('ĐƯỜNG đi'), 'duong di')
})

test('matching is accent- and case-insensitive both ways', () => {
  assert.notEqual(matchScore('lich', 'Lịch'), null)
  assert.notEqual(matchScore('LỊCH', 'lich hop'), null)
  assert.notEqual(matchScore('ghi chu', 'Ghi chú'), null)
  assert.equal(matchScore('lach', 'Lịch'), null)
})

test('empty query matches everything with score 0', () => {
  assert.equal(matchScore('', 'Anything'), 0)
  assert.equal(matchScore('   ', 'Anything'), 0)
})

test('every token must match', () => {
  assert.notEqual(matchScore('pr in', 'PR Inbox'), null)
  assert.equal(matchScore('pr calendar', 'PR Inbox'), null)
})

test('prefix beats word start beats substring', () => {
  const prefix = matchScore('cal', 'Calendar')
  const wordStart = matchScore('cal', 'Google Calendar')
  const substring = matchScore('cal', 'Local files')
  assert.ok(prefix > wordStart, `${prefix} > ${wordStart}`)
  assert.ok(wordStart > substring, `${wordStart} > ${substring}`)
})

test('a later word-start occurrence wins over an earlier substring one', () => {
  // "ar" is inside "Radar" first, then starts "Archive".
  assert.equal(matchScore('ar', 'Radar Archive'), matchScore('ar', 'Old Archive'))
})

test('keywords match but never outrank the label', () => {
  assert.notEqual(matchScore('devdy', 'Fix login', ['Devdy']), null)
  const viaLabel = matchScore('fix', 'Fix login')
  const viaKeyword = matchScore('fix', 'Login', ['fix'])
  assert.ok(viaLabel > viaKeyword)
})

test('rankItems keeps group order and sorts by score inside a group', () => {
  const items = [
    { group: 'screens', label: 'Local calendar sync' },
    { group: 'tabs', label: 'Calendar' },
    { group: 'screens', label: 'Calendar' },
    { group: 'screens', label: 'Notes' },
    { group: 'sessions', label: 'Fix calendar bug' },
  ]
  const out = rankItems(items, 'cal', ['tabs', 'sessions', 'screens'])
  assert.deepEqual(
    out.map((i) => `${i.group}:${i.label}`),
    ['tabs:Calendar', 'sessions:Fix calendar bug', 'screens:Calendar', 'screens:Local calendar sync'],
  )
})

test('rankItems keeps original order for ties (e.g. most recent session first)', () => {
  const items = [
    { group: 's', label: 'Session b' },
    { group: 's', label: 'Session a' },
    { group: 's', label: 'Session c' },
  ]
  assert.deepEqual(
    rankItems(items, 'session').map((i) => i.label),
    ['Session b', 'Session a', 'Session c'],
  )
  assert.deepEqual(
    rankItems(items, '').map((i) => i.label),
    ['Session b', 'Session a', 'Session c'],
  )
})

test('rankItems puts groups missing from groupOrder last', () => {
  const items = [
    { group: 'x', label: 'Alpha' },
    { group: 'a', label: 'Alpha' },
  ]
  assert.deepEqual(rankItems(items, 'al', ['a']).map((i) => i.group), ['a', 'x'])
})
