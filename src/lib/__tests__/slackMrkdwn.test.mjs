// Unit tests for the Slack mrkdwn renderer (lib/slackMrkdwnCore.ts).
// The repo has no JS test framework, so these use Node's built-in runner with
// its TypeScript type stripping (Node ≥ 22.18):
//   node --test src/lib/__tests__/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mrkdwnToHtml, mrkdwnToPlain, replaceEmoji } from '../slackMrkdwnCore.ts'

const ctx = {
  users: {
    U1: { displayName: 'Thai', realName: 'Nguyen Phuong Thai' },
    U2: { displayName: 'Ken', realName: null },
  },
  channels: { C111: 'dev-backend' },
  workspaceDomain: 'malme.slack.com',
}

const html = (text) => mrkdwnToHtml(text, ctx)

test('code block is rendered verbatim, nothing inside is formatted', () => {
  const out = html('before\n```\nconst a = *b* <@U1> &lt;div&gt; :smile:\n```\nafter')
  assert.match(out, /<pre[^>]*><code>const a = \*b\* &lt;@U1&gt; &lt;div&gt; :smile:<\/code><\/pre>/)
  assert.ok(!out.includes('<strong>'))
  // The block eats its surrounding newlines (pre-wrap would add blank lines).
  assert.match(out, /^before<pre/)
  assert.match(out, /<\/pre>after$/)
})

test('inline code is not formatted', () => {
  const out = html('run `npm *run* dev` now')
  assert.match(out, /<code[^>]*>npm \*run\* dev<\/code>/)
  assert.ok(!out.includes('<strong>'))
})

test('bold / italic / strike on word boundaries', () => {
  assert.match(html('this is *bold* text'), /this is <strong>bold<\/strong> text/)
  assert.match(html('_italic_ start'), /^<em>italic<\/em> start/)
  assert.match(html('a ~gone~.'), /a <del>gone<\/del>\./)
  // Not a boundary: inside words / snake_case / math.
  assert.equal(html('snake_case_name'), 'snake_case_name')
  assert.equal(html('2*3*4'), '2*3*4')
  // A marker followed by a space does not open.
  assert.equal(html('* not bold*'), '* not bold*')
  // Never across lines.
  assert.ok(!html('*a\nb*').includes('<strong>'))
})

test('quote lines become one blockquote (escaped &gt; form)', () => {
  const out = html('&gt; line one\n&gt; line two\nafter')
  assert.match(out, /^<blockquote[^>]*>line one\nline two<\/blockquote>after$/)
})

test('lists stay as typed', () => {
  assert.equal(html('• one\n- two\n1. three'), '• one\n- two\n1. three')
})

test('user mentions resolve to display name with real-name tooltip', () => {
  const out = html('hi <@U1> and <@U2|ken> and <@U9|ghost>')
  assert.match(out, /<span class="slack-mention[^"]*" title="Nguyen Phuong Thai">@Thai<\/span>/)
  assert.match(out, /<span class="slack-mention[^"]*">@Ken<\/span>/)
  assert.match(out, /@ghost<\/span>/)
})

test('channel mentions link to the workspace archive', () => {
  assert.match(
    html('see <#C111>'),
    /<a class="slack-link[^"]*" href="https:\/\/malme\.slack\.com\/archives\/C111">#dev-backend<\/a>/,
  )
  assert.match(html('see <#C222|ops>'), />#ops<\/a>/)
  const noDomain = mrkdwnToHtml('<#C111>', { ...ctx, workspaceDomain: null })
  assert.match(noDomain, /<span class="slack-mention[^"]*">#dev-backend<\/span>/)
})

test('special mentions', () => {
  assert.match(html('<!here>'), />@here<\/span>/)
  assert.match(html('<!channel>'), />@channel<\/span>/)
  assert.match(html('<!everyone>'), />@everyone<\/span>/)
  assert.match(html('<!subteam^S123|@backend>'), />@backend<\/span>/)
  assert.equal(html('<!date^1392734382^{date}|Feb 18>'), 'Feb 18')
})

test('links: labelled, bare, mailto; entities in url decoded then escaped', () => {
  assert.match(html('<https://x.io/a?b=1&amp;c=2|the doc>'), /<a [^>]*href="https:\/\/x\.io\/a\?b=1&amp;c=2">the doc<\/a>/)
  assert.match(html('<https://x.io>'), /href="https:\/\/x\.io">https:\/\/x\.io<\/a>/)
  assert.match(html('<mailto:a@b.co|a@b.co>'), /href="mailto:a@b\.co">a@b\.co<\/a>/)
  // Non-http schemes are not links.
  assert.ok(!html('<javascript:alert(1)|x>').includes('<a'))
})

test('emoji shortcodes, skin tones, unknown kept', () => {
  assert.equal(html(':+1: ok :pray:'), '👍 ok 🙏')
  assert.equal(replaceEmoji(':+1::skin-tone-3:'), '👍\u{1F3FC}')
  assert.equal(html(':custom_party:'), ':custom_party:')
  // Times are not emoji.
  assert.equal(html('at 10:30:45'), 'at 10:30:45')
})

test('entities decode and HTML stays inert', () => {
  assert.equal(html('a &lt;b&gt; &amp; c'), 'a &lt;b&gt; &amp; c')
  const out = html('&lt;script&gt;alert(1)&lt;/script&gt; <img src=x onerror=alert(1)>')
  assert.ok(!out.includes('<script'))
  assert.ok(!out.includes('<img'))
})

test('formatting wraps mentions', () => {
  assert.match(html('*<@U1>*'), /^<strong><span[^>]*>@Thai<\/span><\/strong>$/)
})

test('plain text for the clipboard', () => {
  assert.equal(
    mrkdwnToPlain('hi <@U1>, see <#C111> and <https://x.io|doc> &amp; <https://y.io> :tada:', ctx),
    'hi @Thai, see #dev-backend and doc (https://x.io) & https://y.io 🎉',
  )
  assert.equal(mrkdwnToPlain('`<@U1>` stays', ctx), '`<@U1>` stays')
})
