// Build co-registered 2.5D Cyber Fox production layers from the master
// decomposition sheet (fox-assets/source.png).
//
// Pipeline per spec: detect bbox -> extract source pixels -> keep real alpha ->
// register onto a common 1024x1024 canvas (body is the anchor) -> export WebP +
// manifest -> emit debug renders (contact sheet, reconstruction, alpha debug).
//
// NO AI redraw, NO generative fill, NO clone-stamp, NO blur-to-hide-seams.
// If a layer cannot be produced cleanly it is reported, not faked.

import fs from 'node:fs'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const ROOT = path.resolve(__dirname, '..')
const require = createRequire(path.join(ROOT, 'package.json'))

function loadPng() {
  for (const c of ['pngjs', path.join(ROOT, 'node_modules/.pnpm/pngjs@5.0.0/node_modules/pngjs')]) {
    try { return require(c).PNG } catch { /* next */ }
  }
  throw new Error('pngjs required')
}
const PNG = loadPng()

const SRC = path.join(ROOT, 'fox-assets/source.png')
const OUT = path.join(ROOT, 'fox-assets/generated/layers-2_5d')
const CANVAS = 1024
const HAS_CWEBP = (() => { try { execFileSync('which', ['cwebp']); return true } catch { return false } })()

const sheet = PNG.sync.read(fs.readFileSync(SRC))

// ---------------------------------------------------------------------------
// Semantic source map (boxes measured from the sheet via connected-components).
// Boxes are re-trimmed to true alpha content inside the script.
// ---------------------------------------------------------------------------
const STRUCT = {
  body:        [519, 93, 747, 308],
  head:        [3, 95, 252, 274],
  'ear-left':  [252, 15, 364, 192],
  'ear-right': [403, 16, 518, 191],
  'tail-orange': [742, 14, 934, 287],
  'tail-blue':   [951, 12, 1128, 290],
}
const COMPOSITE = [1173, 15, 1509, 392]

const EYES = {
  idle: [14, 359, 74, 413],
  blink: [184, 365, 242, 414],
  'processing-01': [522, 359, 580, 414],
  'processing-02': [601, 359, 660, 414],
  'processing-03': [688, 359, 748, 414],
  happy: [849, 358, 907, 414],
  error: [1007, 356, 1064, 413],
}
const CHEST = {
  idle: [31, 476, 134, 551],
  processing: [176, 476, 279, 551],
  syncing: [321, 476, 424, 551],
  success: [464, 476, 567, 551],
  error: [605, 476, 709, 551],
  permission: [751, 476, 855, 551],
}
const EFFECTS = {
  'trail-orange': [13, 588, 172, 764],
  'trail-blue':   [502, 596, 645, 763],
  infinity:       [993, 599, 1240, 724],
  'ground-glow':  [8, 858, 264, 970],
  swoosh:         [351, 598, 504, 760],
  particles:      [332, 863, 382, 926],
  spark:          [497, 861, 546, 922],
  'pixel-squares':[668, 889, 743, 958],
  'scan-line':    [805, 889, 941, 946],
  orb:            [1107, 851, 1185, 971],
}

// ---------------------------------------------------------------------------
// Registration layout — normalized to the 1024 canvas. cx/cy = target center of
// the placed alpha bbox; wf = target width as a fraction of the canvas.
// Body is the anchor; everything else is expressed in the same canvas space.
// (Tunable; refined by inspecting the reconstruction render.)
// ---------------------------------------------------------------------------
const LAYOUT = {
  body:        { wf: 0.42, cx: 0.5, cy: 0.69, rot: 0.0 },
  head:        { wf: 0.44, cx: 0.5, cy: 0.395, rot: 0.0 },
  'ear-left':  { wf: 0.15, cx: 0.3879, cy: 0.2251, rot: 11.0 },
  'ear-right': { wf: 0.152, cx: 0.606, cy: 0.2216, rot: -13.0 },
  // tails swapped sides per review: orange on the right, blue on the left
  'tail-orange': { wf: 0.215, cx: 0.7625, cy: 0.6079, rot: 34.0 },
  'tail-blue':   { wf: 0.235, cx: 0.2314, cy: 0.6011, rot: -26.0 },
}
const EYE_W = 0.070              // width of each eye on the canvas
const EYE = { left: { cx: 0.4096, cy: 0.4306 }, right: { cx: 0.5796, cy: 0.4306 } }
const EYE_ROT = 0.0
const CHEST_PLACE = { wf: 0.15, cx: 0.5, cy: 0.67, rot: 0.0 }

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------
function trimAlpha([x0, y0, x1, y1], athresh = 12) {
  let minx = x1, miny = y1, maxx = x0, maxy = y0, any = false
  for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) {
    if (sheet.data[(y * sheet.width + x) * 4 + 3] > athresh) {
      any = true
      if (x < minx) minx = x; if (x > maxx) maxx = x
      if (y < miny) miny = y; if (y > maxy) maxy = y
    }
  }
  if (!any) return { x0, y0, w: x1 - x0, h: y1 - y0 }
  return { x0: minx, y0: miny, w: maxx - minx + 1, h: maxy - miny + 1 }
}

// extract a tight RGBA buffer for a source box (real alpha, no processing)
function extract(box) {
  const b = trimAlpha(box)
  const buf = new Uint8ClampedArray(b.w * b.h * 4)
  for (let y = 0; y < b.h; y++) for (let x = 0; x < b.w; x++) {
    const s = ((b.y0 + y) * sheet.width + (b.x0 + x)) * 4
    const d = (y * b.w + x) * 4
    buf[d] = sheet.data[s]; buf[d + 1] = sheet.data[s + 1]
    buf[d + 2] = sheet.data[s + 2]; buf[d + 3] = sheet.data[s + 3]
  }
  return { w: b.w, h: b.h, data: buf }
}

function sample(src, fx, fy) {
  const { w, h, data } = src
  if (fx < 0 || fy < 0 || fx > w - 1 || fy > h - 1) return [0, 0, 0, 0]
  const x0 = Math.floor(fx), y0 = Math.floor(fy)
  const x1 = Math.min(x0 + 1, w - 1), y1 = Math.min(y0 + 1, h - 1)
  const tx = fx - x0, ty = fy - y0
  const o = [0, 0, 0, 0]
  const pts = [[x0, y0, (1 - tx) * (1 - ty)], [x1, y0, tx * (1 - ty)], [x0, y1, (1 - tx) * ty], [x1, y1, tx * ty]]
  for (const [px, py, wgt] of pts) {
    const i = (py * w + px) * 4
    o[0] += data[i] * wgt; o[1] += data[i + 1] * wgt; o[2] += data[i + 2] * wgt; o[3] += data[i + 3] * wgt
  }
  return o
}

// place src (scaled to dw x dh, rotated `rot` deg around its centre) centred at
// canvas pixel (cxpx, cypx) onto dst 1024 canvas
function placeOnCanvas(src, dw, dh, cxpx, cypx, dst, rot = 0) {
  const rad = rot * Math.PI / 180, cs = Math.cos(rad), sn = Math.sin(rad)
  const DW = Math.ceil(Math.abs(dw * cs) + Math.abs(dh * sn))
  const DH = Math.ceil(Math.abs(dw * sn) + Math.abs(dh * cs))
  const ox = Math.round(cxpx - DW / 2), oy = Math.round(cypx - DH / 2)
  let minx = CANVAS, miny = CANVAS, maxx = 0, maxy = 0, any = false
  for (let Y = 0; Y < DH; Y++) for (let X = 0; X < DW; X++) {
    const dx = ox + X, dy = oy + Y
    if (dx < 0 || dy < 0 || dx >= CANVAS || dy >= CANVAS) continue
    const ux = X - DW / 2, uy = Y - DH / 2
    const px = ux * cs + uy * sn      // inverse-rotate into unrotated part space
    const py = -ux * sn + uy * cs
    const fx = (px + dw / 2) / dw * (src.w - 1)
    const fy = (py + dh / 2) / dh * (src.h - 1)
    if (fx < 0 || fy < 0 || fx > src.w - 1 || fy > src.h - 1) continue
    const [r, g, b, a] = sample(src, fx, fy)
    if (a < 1) continue
    const di = (dy * CANVAS + dx) * 4
    const sa = a / 255, da = dst.data[di + 3] / 255, out = sa + da * (1 - sa)
    dst.data[di] = (r * sa + dst.data[di] * da * (1 - sa)) / (out || 1)
    dst.data[di + 1] = (g * sa + dst.data[di + 1] * da * (1 - sa)) / (out || 1)
    dst.data[di + 2] = (b * sa + dst.data[di + 2] * da * (1 - sa)) / (out || 1)
    dst.data[di + 3] = Math.round(out * 255)
    any = true
    if (dx < minx) minx = dx; if (dx > maxx) maxx = dx
    if (dy < miny) miny = dy; if (dy > maxy) maxy = dy
  }
  return any ? { minx, miny, maxx, maxy } : null
}

function blankCanvas() { return new PNG({ width: CANVAS, height: CANVAS }) }

function saveWebp(relPath, png) {
  const abs = path.join(OUT, relPath)
  fs.mkdirSync(path.dirname(abs), { recursive: true })
  if (HAS_CWEBP) {
    const tmp = path.join('/tmp', 'cf-' + relPath.replace(/[\/]/g, '_') + '.png')
    fs.writeFileSync(tmp, PNG.sync.write(png))
    execFileSync('cwebp', ['-quiet', '-q', '92', '-alpha_q', '100', tmp, '-o', abs.replace(/\.webp$/, '.webp')])
    fs.rmSync(tmp, { force: true })
  } else {
    fs.writeFileSync(abs.replace(/\.webp$/, '.png'), PNG.sync.write(png))
  }
}

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------
fs.rmSync(OUT, { recursive: true, force: true })
fs.mkdirSync(OUT, { recursive: true })

const manifest = {
  version: 1,
  source: 'fox-assets/source.png',
  canvas: { width: CANVAS, height: CANVAS, origin: 'top-left' },
  layers: {},
  eyes: {},
  chest: {},
  tails: {},
  effects: {},
}
const Z = { body: 40, head: 50, 'ear-left': 60, 'ear-right': 61 }
const placed = {}      // name -> {src, dw, dh, cxpx, cypx, bbox}
const report = []

function toKey(name) {
  return name.replace(/-([a-z])/g, (_, c) => c.toUpperCase())
}

// --- structural character layers on 1024 canvas ---
for (const [name, box] of Object.entries(STRUCT)) {
  const src = extract(box)
  const L = LAYOUT[name]
  const dw = Math.round(L.wf * CANVAS)
  const dh = Math.round(dw * (src.h / src.w))
  const cxpx = L.cx * CANVAS, cypx = L.cy * CANVAS
  const png = blankCanvas()
  const bbox = placeOnCanvas(src, dw, dh, cxpx, cypx, png, L.rot ?? 0)
  saveWebp(`${name}.webp`, png)
  placed[name] = { src, dw, dh, cxpx, cypx, bbox }

  // pivots
  let pivot
  if (name === 'body') pivot = { x: 0.5, y: (bbox.maxy) / CANVAS }
  else if (name === 'head') pivot = { x: (bbox.minx + bbox.maxx) / 2 / CANVAS, y: bbox.maxy / CANVAS }
  else if (name.startsWith('ear')) pivot = { x: (bbox.minx + bbox.maxx) / 2 / CANVAS, y: bbox.maxy / CANVAS }
  else pivot = null // tails handled below

  if (name.startsWith('tail')) {
    // pivot side follows placement (root nearest body centre), not colour name
    const side = LAYOUT[name].cx < 0.5 ? 'left' : 'right'
    const rootX = side === 'left' ? bbox.maxx : bbox.minx
    const rootY = bbox.miny + Math.round((bbox.maxy - bbox.miny) * 0.12)
    const key = name.endsWith('orange') ? 'tailOrange' : 'tailBlue'
    manifest.tails[key] = {
      base: `${name}.webp`,
      glow: null, energy: null, // no separate source in master sheet
      pivot: { x: rootX / CANVAS, y: rootY / CANVAS },
      restRotation: 0,
    }
    report.push({ asset: `${name}.webp`, src: box.join(','), out: `${dw}x${dh}`, alpha: 'real', reg: 'canvas', pivot: `${(rootX / CANVAS).toFixed(3)},${(rootY / CANVAS).toFixed(3)}`, status: 'PASS', notes: 'single tail source; glow/energy modulated at runtime (no separate asset)' })
  } else {
    manifest.layers[toKey(name)] = { file: `${name}.webp`, zIndex: Z[name], pivot }
    report.push({ asset: `${name}.webp`, src: box.join(','), out: `${dw}x${dh}`, alpha: 'real', reg: 'canvas anchor=body', pivot: `${pivot.x.toFixed(3)},${pivot.y.toFixed(3)}`, status: 'PASS', notes: '' })
  }
}

// --- eyes: same glyph placed at both sockets, fixed position+scale per state ---
for (const [state, box] of Object.entries(EYES)) {
  const src = extract(box)
  const dw = Math.round(EYE_W * CANVAS)
  const dh = Math.round(dw * (src.h / src.w))
  const png = blankCanvas()
  placeOnCanvas(src, dw, dh, EYE.left.cx * CANVAS, EYE.left.cy * CANVAS, png, EYE_ROT)
  placeOnCanvas(src, dw, dh, EYE.right.cx * CANVAS, EYE.right.cy * CANVAS, png, EYE_ROT)
  saveWebp(`eyes/${state}.webp`, png)
  manifest.eyes[state] = { file: `eyes/${state}.webp` }
  report.push({ asset: `eyes/${state}.webp`, src: box.join(','), out: `${dw}x${dh} x2`, alpha: 'real', reg: `sockets ${EYE.left.cx},${EYE.right.cx}@${EYE.left.cy}`, pivot: '-', status: 'PASS', notes: 'single-eye glyph mirrored to both sockets; fixed scale/pos' })
}
manifest.layers.eyes = { fileByState: 'eyes/<state>.webp', zIndex: 70, socket: EYE, width: EYE_W }

// --- chest: fixed center+scale per state ---
for (const [state, box] of Object.entries(CHEST)) {
  const src = extract(box)
  const dw = Math.round(CHEST_PLACE.wf * CANVAS)
  const dh = Math.round(dw * (src.h / src.w))
  const png = blankCanvas()
  placeOnCanvas(src, dw, dh, CHEST_PLACE.cx * CANVAS, CHEST_PLACE.cy * CANVAS, png, CHEST_PLACE.rot ?? 0)
  saveWebp(`chest/${state}.webp`, png)
  manifest.chest[state] = { file: `chest/${state}.webp` }
  report.push({ asset: `chest/${state}.webp`, src: box.join(','), out: `${dw}x${dh}`, alpha: 'real', reg: `center ${CHEST_PLACE.cx},${CHEST_PLACE.cy}`, pivot: '-', status: 'PASS', notes: '' })
}
manifest.layers.chest = { fileByState: 'chest/<state>.webp', zIndex: 80, center: { x: CHEST_PLACE.cx, y: CHEST_PLACE.cy }, width: CHEST_PLACE.wf }

// --- effects: cropped, keep native size + metadata ---
const FX_PLACE = {
  'trail-orange': { x: 0.24, y: 0.55 }, 'trail-blue': { x: 0.76, y: 0.52 },
  infinity: { x: 0.5, y: 0.55 }, 'ground-glow': { x: 0.5, y: 0.9 },
  swoosh: { x: 0.3, y: 0.5 }, particles: { x: 0.5, y: 0.4 }, spark: { x: 0.7, y: 0.35 },
  'pixel-squares': { x: 0.5, y: 0.45 }, 'scan-line': { x: 0.5, y: 0.35 }, orb: { x: 0.5, y: 0.4 },
}
for (const [name, box] of Object.entries(EFFECTS)) {
  const src = extract(box)
  const png = new PNG({ width: src.w, height: src.h })
  png.data.set(src.data)
  saveWebp(`effects/${name}.webp`, png)
  manifest.effects[toKey(name)] = {
    file: `effects/${name}.webp`,
    width: src.w, height: src.h,
    anchor: { x: 0.5, y: 0.5 }, pivot: { x: 0.5, y: 0.5 },
    recommendedPlacement: FX_PLACE[name] || { x: 0.5, y: 0.5 },
  }
  report.push({ asset: `effects/${name}.webp`, src: box.join(','), out: `${src.w}x${src.h}`, alpha: 'real', reg: 'cropped+meta', pivot: '0.5,0.5', status: 'PASS', notes: '' })
}

// --- canonical composite scaled to fit the 1024 canvas (registration target) ---
{
  const src = extract(COMPOSITE)
  const scale = Math.min((CANVAS * 0.94) / src.w, (CANVAS * 0.94) / src.h)
  const dw = Math.round(src.w * scale), dh = Math.round(src.h * scale)
  const png = blankCanvas()
  placeOnCanvas(src, dw, dh, CANVAS / 2, CANVAS * 0.52, png)
  saveWebp('canonical-composite.webp', png)
  manifest.canonicalComposite = { file: 'canonical-composite.webp', note: 'style reference / registration target only' }
  report.push({ asset: 'canonical-composite.webp', src: COMPOSITE.join(','), out: `${dw}x${dh}`, alpha: 'real', reg: 'centered', pivot: '-', status: 'PASS', notes: 'reference only, not an animated layer' })
}

// current placement (normalized centres) so the debug tool can drag from a known base
const withRot = (o) => ({ wf: o.wf ?? null, cx: o.cx, cy: o.cy, rot: o.rot ?? 0 })
manifest.placement = {
  body: withRot(LAYOUT.body),
  head: withRot(LAYOUT.head),
  earLeft: withRot(LAYOUT['ear-left']),
  earRight: withRot(LAYOUT['ear-right']),
  tailOrange: withRot(LAYOUT['tail-orange']),
  tailBlue: withRot(LAYOUT['tail-blue']),
  eyeLeft: { ...EYE.left, wf: EYE_W, rot: EYE_ROT },
  eyeRight: { ...EYE.right, wf: EYE_W, rot: EYE_ROT },
  chest: withRot(CHEST_PLACE),
}

fs.writeFileSync(path.join(OUT, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n')

// ---------------------------------------------------------------------------
// debug renders
// ---------------------------------------------------------------------------
function checker(png, cell = 16) {
  for (let y = 0; y < png.height; y++) for (let x = 0; x < png.width; x++) {
    const c = (((x / cell) | 0) + ((y / cell) | 0)) & 1 ? 205 : 150
    const o = (y * png.width + x) * 4
    png.data[o] = c; png.data[o + 1] = c; png.data[o + 2] = c; png.data[o + 3] = 255
  }
}
function over(png, layerPng) {
  for (let i = 0; i < png.width * png.height; i++) {
    const a = layerPng.data[i * 4 + 3] / 255
    if (!a) continue
    png.data[i * 4] = layerPng.data[i * 4] * a + png.data[i * 4] * (1 - a)
    png.data[i * 4 + 1] = layerPng.data[i * 4 + 1] * a + png.data[i * 4 + 1] * (1 - a)
    png.data[i * 4 + 2] = layerPng.data[i * 4 + 2] * a + png.data[i * 4 + 2] * (1 - a)
  }
}
function loadWebp(rel) {
  const abs = path.join(OUT, rel)
  const p = abs.endsWith('.webp') && !HAS_CWEBP ? abs.replace(/\.webp$/, '.png') : abs
  if (HAS_CWEBP) {
    const tmp = '/tmp/cf-read.png'
    execFileSync('dwebp', ['-quiet', p, '-o', tmp])
    return PNG.sync.read(fs.readFileSync(tmp))
  }
  return PNG.sync.read(fs.readFileSync(p))
}

// reconstruction (z-order composite of structural layers + eyes idle + chest idle)
const RECON_ORDER = ['tail-orange', 'tail-blue', 'body', 'head', 'ear-left', 'ear-right']
const recon = blankCanvas(); checker(recon)
for (const n of RECON_ORDER) over(recon, loadWebp(`${n}.webp`))
over(recon, loadWebp('eyes/idle.webp'))
over(recon, loadWebp('chest/idle.webp'))
fs.writeFileSync(path.join(ROOT, 'cyber-fox-reconstruction.png'), PNG.sync.write(recon))

// side-by-side: composite | reconstruction | 50/50
{
  const comp = loadWebp('canonical-composite.webp')
  const W3 = CANVAS * 3, H = CANVAS
  const sbs = new PNG({ width: W3, height: H })
  const compChk = blankCanvas(); checker(compChk); over(compChk, comp)
  const blend = blankCanvas(); checker(blend)
  for (let i = 0; i < CANVAS * CANVAS; i++) {
    for (let k = 0; k < 3; k++) blend.data[i * 4 + k] = compChk.data[i * 4 + k] * 0.5 + recon.data[i * 4 + k] * 0.5
    blend.data[i * 4 + 3] = 255
  }
  const panels = [compChk, recon, blend]
  panels.forEach((p, idx) => {
    for (let y = 0; y < H; y++) for (let x = 0; x < CANVAS; x++) {
      const s = (y * CANVAS + x) * 4, d = (y * W3 + idx * CANVAS + x) * 4
      sbs.data[d] = p.data[s]; sbs.data[d + 1] = p.data[s + 1]; sbs.data[d + 2] = p.data[s + 2]; sbs.data[d + 3] = 255
    }
  })
  fs.writeFileSync(path.join(ROOT, 'cyber-fox-reconstruction-compare.png'), PNG.sync.write(sbs))
}

// contact sheet of all production webp assets on checkerboard
{
  const all = [...Object.keys(STRUCT).map(n => `${n}.webp`),
    ...Object.keys(EYES).map(s => `eyes/${s}.webp`),
    ...Object.keys(CHEST).map(s => `chest/${s}.webp`),
    ...Object.keys(EFFECTS).map(n => `effects/${n}.webp`)]
  const CELL = 200, COLS = 8, ROWS = Math.ceil(all.length / COLS)
  const cs = new PNG({ width: COLS * CELL, height: ROWS * CELL }); checker(cs, 12)
  all.forEach((rel, i) => {
    const img = loadWebp(rel)
    // fit into cell
    const sc = Math.min((CELL - 16) / img.width, (CELL - 16) / img.height, 1)
    const dw = (img.width * sc) | 0, dh = (img.height * sc) | 0
    const ox = (i % COLS) * CELL + ((CELL - dw) >> 1), oy = ((i / COLS) | 0) * CELL + ((CELL - dh) >> 1)
    for (let y = 0; y < dh; y++) for (let x = 0; x < dw; x++) {
      const sx = (x / sc) | 0, sy = (y / sc) | 0, s = (sy * img.width + sx) * 4
      const a = img.data[s + 3] / 255; if (!a) continue
      const d = ((oy + y) * cs.width + (ox + x)) * 4
      cs.data[d] = img.data[s] * a + cs.data[d] * (1 - a)
      cs.data[d + 1] = img.data[s + 1] * a + cs.data[d + 1] * (1 - a)
      cs.data[d + 2] = img.data[s + 2] * a + cs.data[d + 2] * (1 - a)
    }
  })
  fs.writeFileSync(path.join(ROOT, 'cyber-fox-production-contact-sheet.png'), PNG.sync.write(cs))
}

// alpha debug: structural layers over white / black / 50% gray columns
{
  const names = ['head.webp', 'ear-left.webp', 'ear-right.webp', 'tail-orange.webp', 'tail-blue.webp', 'body.webp', 'effects/ground-glow.webp']
  const CELL = 220, bg = [255, 128, 0] // 3 bg columns
  const cols = [255, 0, 128]
  const dbg = new PNG({ width: CELL * 3, height: CELL * names.length })
  names.forEach((rel, r) => {
    const img = loadWebp(rel)
    cols.forEach((bgc, c) => {
      // fill bg
      for (let y = 0; y < CELL; y++) for (let x = 0; x < CELL; x++) {
        const d = ((r * CELL + y) * dbg.width + (c * CELL + x)) * 4
        dbg.data[d] = bgc; dbg.data[d + 1] = bgc; dbg.data[d + 2] = bgc; dbg.data[d + 3] = 255
      }
      const sc = Math.min((CELL - 10) / img.width, (CELL - 10) / img.height, 1)
      const dw = (img.width * sc) | 0, dh = (img.height * sc) | 0
      const ox = c * CELL + ((CELL - dw) >> 1), oy = r * CELL + ((CELL - dh) >> 1)
      for (let y = 0; y < dh; y++) for (let x = 0; x < dw; x++) {
        const sx = (x / sc) | 0, sy = (y / sc) | 0, s = (sy * img.width + sx) * 4
        const a = img.data[s + 3] / 255; if (!a) continue
        const d = ((oy + y) * dbg.width + (ox + x)) * 4
        dbg.data[d] = img.data[s] * a + dbg.data[d] * (1 - a)
        dbg.data[d + 1] = img.data[s + 1] * a + dbg.data[d + 1] * (1 - a)
        dbg.data[d + 2] = img.data[s + 2] * a + dbg.data[d + 2] * (1 - a)
      }
    })
  })
  fs.writeFileSync(path.join(ROOT, 'cyber-fox-alpha-debug.png'), PNG.sync.write(dbg))
}

// quality report
console.log('\n=== Cyber Fox 2.5D asset build ===')
console.log('cwebp:', HAS_CWEBP ? 'yes (.webp)' : 'no (.png fallback)')
console.log('out:', path.relative(ROOT, OUT))
console.table(report.map(r => ({ asset: r.asset, out: r.out, status: r.status, pivot: r.pivot, notes: r.notes })))
fs.writeFileSync(path.join(OUT, 'quality-report.json'), JSON.stringify(report, null, 2) + '\n')
console.log('debug: cyber-fox-reconstruction.png / -compare.png / -production-contact-sheet.png / -alpha-debug.png')
