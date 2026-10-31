// Pad (or, at the same size, clean the corners of) a square 32-bit BMP to `size` x `size` by clamping to its edge pixels, so
// the maskable icon's background continues the artwork's own gradient instead
// of showing a seam. Used by gen-pwa-icons.sh (sips does the PNG <-> BMP part).
// Usage: node pwa-icon-extend.mjs in.bmp out.bmp size
import { readFileSync, writeFileSync } from 'node:fs'

const [input, output, sizeArg] = process.argv.slice(2)
const size = Number(sizeArg)
const src = readFileSync(input)
const off = src.readUInt32LE(10)
const w = src.readInt32LE(18)
const h = src.readInt32LE(22)
if (src.readUInt16LE(28) !== 32) throw new Error('expected a 32-bit BMP')
const topDown = h < 0
const ah = Math.abs(h)

// Sample the padding from a few pixels inside the edge: the artwork's corners
// are the anti-aliased rim of a rounded square, not background.
const INSET = 12
function pixel(x, y) {
  const corner = (x < INSET || x >= w - INSET) && (y < INSET || y >= ah - INSET)
  const inside = x >= 0 && y >= 0 && x < w && y < ah && !corner
  const lo = inside ? 0 : INSET
  const cx = Math.min(w - 1 - lo, Math.max(lo, x))
  const cy = Math.min(ah - 1 - lo, Math.max(lo, y))
  const row = topDown ? cy : ah - 1 - cy
  const i = off + (row * w + cx) * 4
  return src.subarray(i, i + 4)
}

const pad = Math.floor((size - w) / 2)
const header = Buffer.alloc(54)
header.write('BM', 0)
header.writeUInt32LE(54 + size * size * 4, 2)
header.writeUInt32LE(54, 10)
header.writeUInt32LE(40, 14)
header.writeInt32LE(size, 18)
header.writeInt32LE(-size, 22)
header.writeUInt16LE(1, 26)
header.writeUInt16LE(32, 28)
header.writeUInt32LE(size * size * 4, 34)
const body = Buffer.alloc(size * size * 4)
for (let y = 0; y < size; y++) {
  for (let x = 0; x < size; x++) {
    const p = pixel(x - pad, y - pad)
    const o = (y * size + x) * 4
    body[o] = p[0]
    body[o + 1] = p[1]
    body[o + 2] = p[2]
    body[o + 3] = 255
  }
}
writeFileSync(output, Buffer.concat([header, body]))
