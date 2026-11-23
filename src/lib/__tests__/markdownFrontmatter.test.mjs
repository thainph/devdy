// Unit tests for lib/markdownFrontmatter.ts (FileViewer markdown preview).
// Run: node --test src/lib/__tests__/markdownFrontmatter.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'

const { splitFrontmatter, frontmatterHtml } = await import('../markdownFrontmatter.ts')

test('splitFrontmatter: splits a leading YAML block off the body', () => {
  const md = '---\ntitle: "A"\norder: 1\n---\n# Heading\n'
  assert.deepEqual(splitFrontmatter(md), { frontmatter: 'title: "A"\norder: 1', body: '# Heading\n' })
  assert.deepEqual(splitFrontmatter('---\r\na: 1\r\n---\r\nx'), { frontmatter: 'a: 1', body: 'x' })
  assert.deepEqual(splitFrontmatter('﻿---\na: 1\n---'), { frontmatter: 'a: 1', body: '' })
})

test('splitFrontmatter: leaves files without frontmatter alone', () => {
  assert.equal(splitFrontmatter('# A\n\n---\nb: 1\n---\n').frontmatter, null)
  assert.equal(splitFrontmatter('---\nunterminated: 1\n').frontmatter, null)
  assert.equal(splitFrontmatter('text\n---\n').frontmatter, null)
})

test('frontmatterHtml: flat pairs become an escaped key/value table', () => {
  const html = frontmatterHtml('pms_title: "1 — <b>"\nstatus: draft\n# note')
  assert.equal(html, '<table class="frontmatter"><tbody><tr><th>pms_title</th><td>1 — &lt;b&gt;</td></tr><tr><th>status</th><td>draft</td></tr></tbody></table>')
})

test('frontmatterHtml: nested YAML falls back to a code block', () => {
  for (const yaml of ['tags:\n  - a\n  - b', 'desc: |\n  long', 'meta: {a: 1}']) {
    assert.match(frontmatterHtml(yaml), /^<pre class="frontmatter"><code class="language-yaml">/)
  }
})
