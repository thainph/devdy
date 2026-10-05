/** A todo / note's one-line label: the first non-empty line, stripped of markdown noise. */
export function firstLine(text: string): string {
  const line = text.split('\n').find((l) => l.trim()) ?? ''
  return line
    .replace(/^\s*[-*+]\s+\[[ xX]\]\s*/, '') // checklist marker
    .replace(/^\s*[-*+]\s+/, '') // bullet
    .replace(/^\s*#{1,6}\s+/, '') // heading
    .replace(/^\s*>\s?/, '') // quote
    .trim()
}
