// QA tests (independent of the dev tests): renderer edge cases + XSS, and the
// client-side Slack link check vs. spec §4 / the Rust parser.
// Run: node --test src/lib/__tests__/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mrkdwnToHtml, mrkdwnToPlain } from '../slackMrkdwnCore.ts'
import { checkThreadLink, groupMessages, groupThreads } from '../slackThreadUtils.ts'

const ctx = {
  users: { U1: { displayName: 'Thai', realName: 'Nguyen <b>Thai</b>' } },
  channels: { C111: 'dev' },
  workspaceDomain: 'malme.slack.com',
}
const html = (s) => mrkdwnToHtml(s, ctx)

/** No raw tag other than the renderer's own, no event handler attribute, no js: href. */
function assertInert(out) {
  const tags = [...out.matchAll(/<\/?([a-zA-Z][a-zA-Z0-9]*)/g)].map((m) => m[1].toLowerCase())
  const allowed = new Set(['a', 'span', 'strong', 'em', 'del', 'code', 'pre', 'blockquote'])
  for (const t of tags) assert.ok(allowed.has(t), `unexpected tag <${t}> in ${out}`)
  // Attributes only inside tags: strip text content first.
  // Attribute values are quoted with escaped content: drop them before looking for handlers.
  const inTags = [...out.matchAll(/<[a-zA-Z][^>]*>/g)].map((m) => m[0].replace(/"[^"]*"/g, '""')).join(' ')
  assert.ok(!/\son[a-z]+\s*=/i.test(inTags), `event handler attr in ${out}`)
  for (const m of out.matchAll(/href="([^"]*)"/g)) {
    assert.match(m[1], /^(https?:\/\/|mailto:)/i, `unsafe href ${m[1]}`)
  }
}

test('XSS: escaped script tag stays text', () => {
  const out = html('&lt;script&gt;alert(1)&lt;/script&gt;')
  assert.equal(out, '&lt;script&gt;alert(1)&lt;/script&gt;')
  assertInert(out)
})

test('XSS: javascript: link is not a link', () => {
  for (const s of ['<javascript:alert(1)|x>', '<JavaScript:alert(1)>', '<data:text/html,<b>|x>', '<vbscript:x|y>']) {
    const out = html(s)
    assert.ok(!out.includes('<a'), `${s} → ${out}`)
    assertInert(out)
  }
})

test('XSS: quote in url cannot break out of href', () => {
  const out = html('<https://x" onmouseover="alert(1)|y>')
  assert.ok(out.includes('href="https://x&quot; onmouseover=&quot;alert(1)"'), out)
  assertInert(out)
})

test('XSS: html in label / realName / channel label is escaped', () => {
  for (const s of [
    '<https://x.io|&lt;img src=x onerror=alert(1)&gt;>',
    '<https://x.io|"><img src=x>>',
    '<@U1>',
    '<#C222|&lt;svg onload=alert(1)&gt;>',
    '<@U9|&lt;img src=x onerror=1&gt;>',
    '<!subteam^S1|&lt;img src=x&gt;>',
    '```&lt;img src=x onerror=alert(1)&gt;```',
    '`&lt;img src=x&gt;`',
    '*&lt;img src=x&gt;*',
    '&gt; &lt;iframe src=x&gt;',
  ]) {
    assertInert(html(s))
  }
  assert.match(html('<@U1>'), /title="Nguyen &lt;b&gt;Thai&lt;\/b&gt;"/)
})

test('code block content is not processed', () => {
  const out = html('```*b* _i_ ~s~ <https://x.io|l> <!here> :smile: &amp;```')
  assert.ok(!/<(strong|em|del|a|span)\b/.test(out.replace(/<pre[^>]*><code>/, '')), out)
  assert.ok(out.includes(':smile:'))
  assert.ok(out.includes('&amp;'))
})

test('emphasis boundaries (Slack-like)', () => {
  assert.equal(html('a*b*c'), 'a*b*c')
  assert.equal(html('file_name_here'), 'file_name_here')
  assert.equal(html('x ~y~z'), 'x ~y~z')
  assert.match(html('(*bold*)'), /\(<strong>bold<\/strong>\)/)
  assert.match(html('*bold*, and _it_!'), /<strong>bold<\/strong>, and <em>it<\/em>!/)
})

// F-4: markers right inside quotes / next to escaped entities format like Slack.
test('emphasis opens / closes next to a double quote', () => {
  assert.equal(html('"*q*"'), '&quot;<strong>q</strong>&quot;')
  assert.match(html('say "_hi_" ok'), /say &quot;<em>hi<\/em>&quot; ok/)
  assert.match(html('"~gone~"'), /<del>gone<\/del>/)
  assert.match(html('*a*&amp;b'), /<strong>a<\/strong>&amp;b/)
  // Still not inside words.
  assert.equal(html('a"*b*c'), 'a&quot;*b*c')
})

test('blockquote with raw > and &gt;', () => {
  assert.match(html('> raw'), /^<blockquote[^>]*>raw<\/blockquote>$/)
  assert.match(html('&gt; esc'), /^<blockquote[^>]*>esc<\/blockquote>$/)
  // `>` mid-line is not a quote.
  assert.ok(!html('a &gt; b').includes('blockquote'))
})

test('channel link and url schemes', () => {
  assert.match(html('<#C111>'), /href="https:\/\/malme\.slack\.com\/archives\/C111">#dev</)
  assert.match(html('<http://x.io>'), /href="http:\/\/x\.io"/)
  assert.match(html('<mailto:a@b.co>'), /href="mailto:a@b\.co">a@b\.co</)
})

test('plain text: mention → @Name, entities decoded', () => {
  assert.equal(mrkdwnToPlain('<@U1> &lt;ok&gt;', ctx), '@Thai <ok>')
})

// ── §4 link check (table + edges), compared with the Rust parser behavior ──

const B = 'https://malme.slack.com/archives'
test('link check: spec §4 table', () => {
  assert.equal(checkThreadLink(`${B}/C0B83GHDTFG/p1789877283026359`), 'ok')
  assert.equal(
    checkThreadLink(`${B}/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG`),
    'ok',
  )
  assert.equal(
    checkThreadLink(`${B}/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C999`),
    'invalid_link',
  )
  assert.equal(checkThreadLink(`${B}/D01ABCDEF/p1789877283026359`), 'dm_unsupported')
})

test('link check: edges agreeing with Rust', () => {
  assert.equal(checkThreadLink(`  <${B}/C0B83GHDTFG/p1789877283026359>  `), 'ok')
  assert.equal(checkThreadLink('http://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359'), 'ok')
  assert.equal(checkThreadLink('HTTPS://MALME.SLACK.COM/archives/C0B83GHDTFG/p1789877283026359'), 'ok')
  assert.equal(checkThreadLink('https://acme.enterprise.slack.com/archives/C0B83GHDTFG/p1789877283026359'), 'ok')
  assert.equal(checkThreadLink(`${B}/C0B83GHDTFG/p1789877283026359/`), 'ok')
  assert.equal(checkThreadLink(`${B}/C0B83GHDTFG/p1789877283026359#x`), 'ok')
  assert.equal(checkThreadLink(`${B}/G0B83GHDTFG/p1789877283026359`), 'ok')
  assert.equal(checkThreadLink('https://slack.com/archives/C0B83GHDTFG/p1789877283026359'), 'invalid_link')
  assert.equal(checkThreadLink('https://malme.slack.com.evil.io/archives/C0B83GHDTFG/p1789877283026359'), 'invalid_link')
  assert.equal(checkThreadLink('https://-x.slack.com/archives/C0B83GHDTFG/p1789877283026359'), 'invalid_link')
  assert.equal(checkThreadLink('https://a.b.slack.com/archives/C0B83GHDTFG/p1789877283026359'), 'invalid_link')
  assert.equal(checkThreadLink(`${B}/C0B83GHDTFG/1789877283026359`), 'invalid_link') // no p
  assert.equal(checkThreadLink(`${B}/C0B83GHDTFG/p178987728302635`), 'invalid_link') // 15 digits
  assert.equal(checkThreadLink(`${B}/c0b83ghdtfg/p1789877283026359`), 'invalid_link') // lowercase
  assert.equal(checkThreadLink(`${B}/C0B83GH/p1789877283026359`), 'invalid_link') // too short
  assert.equal(checkThreadLink('ftp://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359'), 'invalid_link')
  assert.equal(checkThreadLink('abc'), 'invalid_link')
  assert.equal(checkThreadLink(`${B}/C0B83GHDTFG/p1789877283026359?cid=`), 'invalid_link')
})

// F-3: the FE check is a port of src-tauri/src/slack_threads/link.rs — same verdicts.
test('link check: agrees with the Rust parser on its quirks', () => {
  // Rust ignores a malformed thread_ts (falls back to the path ts) → ok.
  assert.equal(checkThreadLink(`${B}/C0B83GHDTFG/p1789877283026359?thread_ts=abc`), 'ok')
  // Rust keeps the port in the host → invalid_link.
  assert.equal(checkThreadLink('https://malme.slack.com:443/archives/C0B83GHDTFG/p1789877283026359'), 'invalid_link')
  // Rust decides DM from the path before reading the query → dm_unsupported.
  assert.equal(checkThreadLink(`${B}/D01ABCDEF/p1789877283026359?cid=C1`), 'dm_unsupported')
  // cid is percent-decoded before the comparison.
  assert.equal(checkThreadLink(`${B}/C0B83GHDTFG/p1789877283026359?cid=C0B83GHDTF%47`), 'ok')
})

// ── grouping ──
function m(seq, userId, min, botId = null) {
  return {
    ts: String(seq), seq, userId, botId, authorName: 'n', avatarPath: null, subtype: null,
    textRaw: '', textVi: null, translateStatus: 'done', translateError: null, editedTs: null,
    postedAt: new Date(Date.UTC(2026, 0, 1, 0, min)).toISOString(), reactions: [], fileIds: [],
  }
}
test('grouping: parent never groups, first reply never groups, 5 min window, bots by botId', () => {
  const rows = groupMessages([m(0, 'U1', 0), m(1, 'U1', 1), m(2, 'U1', 6), m(3, 'U1', 12), m(4, null, 13, 'B1'), m(5, null, 14, 'B1'), m(6, null, 15, 'B2')])
  assert.deepEqual(rows.map((r) => r.continued), [false, false, true, false, false, true, false])
  assert.equal(rows[0].isParent, true)
})

test('sidebar tree: fallbacks + group order by min(position)', () => {
  const base = { workspace: null, workspaceDomain: null, channel: null, channelId: null }
  const tree = groupThreads([
    { ...base, id: 'a', position: 5, workspace: 'malme', channel: 'dev' },
    { ...base, id: 'b', position: 1, workspaceDomain: 'x.slack.com', channelId: 'C1' },
    { ...base, id: 'c', position: 3 },
    { ...base, id: 'd', position: 0, workspace: 'malme', channel: '#ops' },
    { ...base, id: 'e', position: 2, workspace: 'malme', channel: 'dev' },
  ])
  assert.deepEqual(tree.map((w) => w.label), ['malme', 'x.slack.com', null])
  assert.deepEqual(tree[0].channels.map((c) => c.label), ['#ops', '#dev'])
  assert.deepEqual(tree[0].channels[1].threads.map((t) => t.id), ['e', 'a'])
  assert.equal(tree[1].channels[0].label, '#C1')
  assert.equal(tree[2].channels[0].label, null)
})
