// Markdown helpers for reading long captured documents in slices: an outline of
// headings with their char offsets, and slicing by offset/max_chars or by section.

// A setext underline: only `=` (h1) or only `-` (h2), ≤ 3 leading spaces.
const SETEXT_UNDERLINE = /^ {0,3}(=+|-+)[ \t]*$/;
const ATX_HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$/;

/**
 * Can `line` be the text of a setext heading? It must be a plain paragraph
 * line: not blank, not indented code, not an ATX heading, list item, blockquote,
 * fence, thematic break or itself an underline.
 */
function isParagraphLine(line) {
  if (!line.trim()) return false;
  if (/^ {4,}|^\t/.test(line)) return false; // indented code
  if (/^ {0,3}(#{1,6}([ \t]|$)|>|[-+*][ \t]|\d{1,9}[.)][ \t]|`{3,}|~{3,})/.test(line)) return false;
  if (/^ {0,3}([-*_])([ \t]*\1){2,}[ \t]*$/.test(line)) return false; // thematic break
  return !SETEXT_UNDERLINE.test(line);
}

/**
 * Headings with their char offset in `text` (offset = start of the heading text
 * line). Supports ATX (`#`..`######`) and setext (`Title` over `===` → h1 or
 * `---` → h2, as emitted by Turndown). Lines inside fenced code blocks
 * (``` / ~~~) are skipped. A `---` after a blank line is a thematic break, not
 * a heading, because the line before it isn't paragraph text. A multi-line
 * setext paragraph is reported by its last line (good enough for an outline).
 */
export function outline(text = '') {
  const out = [];
  const lines = String(text).split('\n');
  let offset = 0;
  let fence = null;
  // The previous line when it can still become a setext heading, else null.
  let prev = null;
  for (const line of lines) {
    const f = /^ {0,3}(`{3,}|~{3,})/.exec(line);
    let candidate = null;
    if (f) {
      if (!fence) fence = f[1][0];
      else if (f[1][0] === fence) fence = null;
    } else if (!fence) {
      const h = ATX_HEADING.exec(line);
      const atxText = h && h[2] ? h[2].replace(/[ \t]+#+$/, '').trim() : '';
      if (h && atxText) {
        out.push({ level: h[1].length, text: atxText, offset });
      } else if (prev && SETEXT_UNDERLINE.test(line)) {
        out.push({ level: line.trim()[0] === '=' ? 1 : 2, text: prev.line.trim(), offset: prev.offset });
      } else if (isParagraphLine(line)) {
        candidate = { line, offset };
      }
    }
    prev = candidate;
    offset += line.length + 1;
  }
  return out;
}

/** Find a heading by text, case-insensitive: exact match first, then substring. */
function findSection(headings, section) {
  const q = String(section).trim().toLowerCase();
  return (
    headings.find((h) => h.text.toLowerCase() === q) ||
    headings.find((h) => h.text.toLowerCase().includes(q)) ||
    null
  );
}

/**
 * Slice `text` for reading. Without `section` the window is [offset, offset+maxChars)
 * of the whole text. With `section` it is the heading's section (until the next
 * heading of the same or a higher level) and `offset` is relative to that section.
 * Returns { headings, total, start, end, rangeEnd, sectionHeading, slice, nextOffset }
 * where nextOffset is null when nothing remains.
 */
export function sliceMarkdown(text = '', { maxChars = 20000, offset = 0, section = null } = {}) {
  const body = String(text);
  const headings = outline(body);
  const max = Math.max(1, Math.floor(Number(maxChars) || 20000));
  const off = Math.max(0, Math.floor(Number(offset) || 0));
  let rangeStart = 0;
  let rangeEnd = body.length;
  let sectionHeading = null;
  if (section != null && String(section).trim()) {
    sectionHeading = findSection(headings, section);
    if (!sectionHeading) {
      const known = headings.map((h) => `"${h.text}"`).join(', ') || '(none)';
      throw new Error(`section not found: "${String(section).trim()}". Headings: ${known}`);
    }
    rangeStart = sectionHeading.offset;
    const next = headings.find((h) => h.offset > sectionHeading.offset && h.level <= sectionHeading.level);
    rangeEnd = next ? next.offset : body.length;
  }
  const start = Math.min(rangeStart + off, rangeEnd);
  const end = Math.min(start + max, rangeEnd);
  return {
    headings,
    total: body.length,
    start,
    end,
    rangeEnd,
    sectionHeading,
    slice: body.slice(start, end),
    nextOffset: end < rangeEnd ? end - rangeStart : null,
  };
}
