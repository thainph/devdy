// Slack mrkdwn → sanitized HTML for the Slack threads window. The parsing lives
// in lib/slackMrkdwnCore.ts (pure, unit-tested); this adds the DOMPurify pass so
// nothing from a Slack message can ever inject markup or script.
//
// Render the result inside a `white-space: pre-wrap` container, and intercept
// link clicks (they must open in the OS browser, never navigate the webview).
import DOMPurify from 'dompurify'
import { mrkdwnToHtml, type MrkdwnContext } from '@/lib/slackMrkdwnCore'

export type { MrkdwnContext } from '@/lib/slackMrkdwnCore'
export { mrkdwnToPlain } from '@/lib/slackMrkdwnCore'

export function renderSlackMrkdwn(text: string, ctx: MrkdwnContext): string {
  return DOMPurify.sanitize(mrkdwnToHtml(text, ctx), {
    ALLOWED_TAGS: ['a', 'span', 'strong', 'em', 'del', 'code', 'pre', 'blockquote'],
    ALLOWED_ATTR: ['href', 'class', 'title'],
    ALLOWED_URI_REGEXP: /^(?:https?:|mailto:)/i,
  })
}
