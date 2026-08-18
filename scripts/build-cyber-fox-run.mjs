// Build the co-registered 2.5D "gallop" (side-profile) rig for the Cyber Fox
// mascot from the second decomposition sheet (fox-assets/source_2.png).
//
// Output: fox-assets/generated/layers-2_5d/run-side/*.webp + run.json
// Same discipline as build-cyber-fox-layers.mjs: real alpha, measured pivots,
// NO redraw / generative fill / clone-stamp / blur-to-hide-seams. Far-side legs
// are NOT baked here — the component instantiates the near-leg webp a second
// time with a dark tint (standard quadruped rig, not invented anatomy).

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

const SRC = path.join(ROOT, 'fox-assets/source_3.png')
const OUT = path.join(ROOT, 'fox-assets/generated/layers-2_5d/run-side')
const CANVAS = 1024
const HAS_CWEBP = (() => { try { execFileSync('which', ['cwebp']); return true } catch { return false } })()
const sheet = PNG.sync.read(fs.readFileSync(SRC))

// source_3.png ships on a flat white background with no alpha. Recover real
// transparency by flood-filling the near-white region connected to the border
// (interior whites — the fur ruff — stay opaque because they are enclosed by
// darker fur and never reach the border). Honest matte on a flat bg: no
// clone-stamp, no blur, no invented pixels.
function recoverAlpha(png, tol = 232) {
  const { width: W, height: H, data } = png
  const bg = new Uint8Array(W * H)
  const stack = []
  const isWhite = (i) => data[i * 4] >= tol && data[i * 4 + 1] >= tol && data[i * 4 + 2] >= tol
  const seed = (i) => { if (!bg[i] && isWhite(i)) { bg[i] = 1; stack.push(i) } }
  for (let x = 0; x < W; x++) { seed(x); seed((H - 1) * W + x) }
  for (let y = 0; y < H; y++) { seed(y * W); seed(y * W + W - 1) }
  while (stack.length) {
    const i = stack.pop(), x = i % W, y = (i / W) | 0
    if (x > 0) seed(i - 1); if (x < W - 1) seed(i + 1)
    if (y > 0) seed(i - W); if (y < H - 1) seed(i + W)
  }
  for (let i = 0; i < W * H; i++) if (bg[i]) data[i * 4 + 3] = 0
}
recoverAlpha(sheet)

// ---------------------------------------------------------------------------
// Source boxes measured from source_2.png via connected-components (solid core
// at alpha>=130), padded slightly. trimAlpha re-tightens to true content.
// ---------------------------------------------------------------------------
// Boxes measured on source_3.png (each part on its own grid cell, wide white
// gutters). Background is alpha-0 after recoverAlpha, so trimAlpha tightens
// each box to real content — loose boxes within a gutter are safe.
const BOX = {
  'torso':           [40, 52, 306, 256],
  'head':            [366, 18, 552, 256],
  'ear':             [628, 36, 720, 230],
  'tail-orange':     [1008, 48, 1228, 258],
  'tail-blue':       [1264, 60, 1496, 258],
  'leg-front-upper': [32, 324, 224, 532],
  'leg-front-lower': [340, 360, 480, 532],
  'leg-hind-upper':  [568, 320, 720, 532],
  'leg-hind-lower':  [812, 354, 922, 532],
}

// Hinge joints measured inside each leg tile (normalized within the *trimmed*
// tile). top = the segment's own pivot; bottom = where the next segment links.
const HINGE = {
  'leg-front-upper': { top: { x: 0.233, y: 0.33 }, bottom: { x: 0.60, y: 0.90 } },
  'leg-front-lower': { top: { x: 0.142, y: 0.228 } },
  'leg-hind-upper':  { top: { x: 0.405, y: 0.243 }, bottom: { x: 0.60, y: 0.88 } },
  'leg-hind-lower':  { top: { x: 0.197, y: 0.651 } },
}

// ---------------------------------------------------------------------------
// Placement on the 1024 canvas. Fox faces RIGHT; tails stream back-left.
// Torso/head/ear/tails are placed by centre (cx,cy = centre of placed bbox).
// Legs are placed by SOCKET: the upper segment's hinge lands on a torso socket,
// the lower segment's hinge lands on the upper segment's bottom joint.
// (Tunable; refined against the reconstruction render + debug tool.)
// ---------------------------------------------------------------------------
const PLACE = {
  torso:       { wf: 0.40, cx: 0.560, cy: 0.560, rot: -8 },
  head:        { wf: 0.235, cx: 0.6634, cy: 0.340, rot: -6 },
  // ear layer removed — the head tile already includes the ears
  'tail-orange':      { wf: 0.36, cx: 0.2423, cy: 0.468, rot: 6 },
  'tail-blue':        { wf: 0.36, cx: 0.1996, cy: 0.5991, rot: -4 },
}
// Leg sockets on the torso (canvas-normalized) + per-segment scale/rotation.
// Used only to seed the INITIAL leg pose; per-segment centres are then recorded
// so the debug editor can drag them and LEG_PLACE (below) overrides them.
const LEG = {
  front: {
    socket: { x: 0.700, y: 0.520 },      // shoulder on torso
    upper: { wf: 0.125, rot: 6 },
    lower: { wf: 0.115, rot: 18 },
  },
  hind: {
    socket: { x: 0.470, y: 0.545 },      // hip on torso
    upper: { wf: 0.150, rot: -4 },
    lower: { wf: 0.120, rot: 26 },
  },
}

// Manual per-segment centre overrides (paste values exported from the debug
// editor here). Each: { wf, cx, cy, rot }. When present, the segment is placed
// by this centre instead of the socket solver.
const LEG_PLACE = {
  legFrontUpper: { wf: 0.1725, cx: 0.7215, cy: 0.6997, rot: -19 },
  legFrontLower: { wf: 0.1173, cx: 0.7522, cy: 0.6284, rot: -10 },
  legHindUpper:  { wf: 0.219, cx: 0.4847, cy: 0.7112, rot: -4 },
  legHindLower:  { wf: 0.1152, cx: 0.4106, cy: 0.763, rot: 26 },
}

// Explicit hinge-pivot overrides (canvas-normalized) exported from the debug
// editor. When present, these replace the auto-computed pivots (rotation origins
// for the gallop animation).
const PIVOT = {
  torso:            { x: 0.5, y: 0.5469 },
  head:             { x: 0.646, y: 0.444 },
  ear:              { x: 0.7065, y: 0.3975 },
  'tail-orange':    { x: 0.4569, y: 0.5762 },
  'tail-blue':      { x: 0.4182, y: 0.6127 },
  'leg-front-upper':{ x: 0.6533, y: 0.6511 },
  'leg-front-lower':{ x: 0.7033, y: 0.5979 },
  'leg-hind-upper': { x: 0.4351, y: 0.5892 },
  'leg-hind-lower': { x: 0.4645, y: 0.6621 },
}

// z-order (back -> front). Far-side legs get their own (lower) band at runtime.
const Z = {
  'tail-blue': 10, 'tail-orange': 40,
  torso: 44,
  'leg-hind-upper': 45, 'leg-hind-lower': 6,
  'leg-front-upper': 49, 'leg-front-lower': 8,
  head: 4, ear: 2,
}

// ---------------------------------------------------------------------------
// helpers (shared shape with build-cyber-fox-layers.mjs)
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
// place src (scaled dw x dh, rotated rot deg about its centre) centred at
// canvas (cxpx,cypx). Returns bbox + a mapper for tile-space points -> canvas.
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
    const px = ux * cs + uy * sn
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
// map a tile-space normalized point (nx,ny in [0,1] of the src) to canvas px,
// for the same transform placeOnCanvas used.
function mapPoint(src, dw, dh, cxpx, cypx, rot, nx, ny) {
  const rad = rot * Math.PI / 180, cs = Math.cos(rad), sn = Math.sin(rad)
  const px = (nx - 0.5) * dw, py = (ny - 0.5) * dh
  return { x: cxpx + px * cs - py * sn, y: cypx + px * sn + py * cs }
}
function blankCanvas() { return new PNG({ width: CANVAS, height: CANVAS }) }
function saveWebp(relPath, png) {
  const abs = path.join(OUT, relPath)
  fs.mkdirSync(path.dirname(abs), { recursive: true })
  if (HAS_CWEBP) {
    const tmp = path.join('/tmp', 'cfr-' + relPath.replace(/[\/]/g, '_') + '.png')
    fs.writeFileSync(tmp, PNG.sync.write(png))
    execFileSync('cwebp', ['-quiet', '-q', '92', '-alpha_q', '100', tmp, '-o', abs])
    fs.rmSync(tmp, { force: true })
  } else {
    fs.writeFileSync(abs.replace(/\.webp$/, '.png'), PNG.sync.write(png))
  }
}
const toKey = (n) => n.replace(/-([a-z])/g, (_, c) => c.toUpperCase())
const norm = (p) => ({ x: +(p.x / CANVAS).toFixed(4), y: +(p.y / CANVAS).toFixed(4) })

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------
fs.rmSync(OUT, { recursive: true, force: true })
fs.mkdirSync(OUT, { recursive: true })

const manifest = {
  version: 1,
  source: 'fox-assets/source_3.png',
  rig: 'gallop-side',
  facing: 'right',
  canvas: { width: CANVAS, height: CANVAS, origin: 'top-left' },
  layers: {},
  legs: {},
}
const report = []

function placeCentered(name) {
  const src = extract(BOX[name])
  const P = PLACE[name]
  const dw = Math.round(P.wf * CANVAS)
  const dh = Math.round(dw * (src.h / src.w))
  const cxpx = P.cx * CANVAS, cypx = P.cy * CANVAS
  const png = blankCanvas()
  const bbox = placeOnCanvas(src, dw, dh, cxpx, cypx, png, P.rot ?? 0)
  saveWebp(`${name}.webp`, png)
  return { src, dw, dh, cxpx, cypx, rot: P.rot ?? 0, bbox }
}

// --- torso (anchor) ---
{
  const r = placeCentered('torso')
  manifest.layers.torso = { file: 'torso.webp', zIndex: Z.torso, pivot: { x: 0.5, y: (r.bbox.miny + r.bbox.maxy) / 2 / CANVAS } }
  report.push({ asset: 'torso.webp', out: `${r.dw}x${r.dh}`, status: 'PASS' })
}
// --- head (ears are already baked into the head tile, so no separate ear layer) ---
for (const name of ['head']) {
  const r = placeCentered(name)
  const pivot = { x: (r.bbox.minx) / CANVAS, y: r.bbox.maxy / CANVAS }
  manifest.layers[name] = { file: `${name}.webp`, zIndex: Z[name], pivot }
  report.push({ asset: `${name}.webp`, out: `${r.dw}x${r.dh}`, status: 'PASS' })
}
// --- tails (glow is a screen-blend copy of the base at runtime, no separate asset) ---
for (const name of ['tail-orange', 'tail-blue']) {
  const r = placeCentered(name)
  // tail root = right edge (nearest body), upper third
  const rootX = r.bbox.maxx, rootY = r.bbox.miny + (r.bbox.maxy - r.bbox.miny) * 0.35
  manifest.layers[toKey(name)] = { file: `${name}.webp`, zIndex: Z[name], pivot: { x: rootX / CANVAS, y: rootY / CANVAS } }
  report.push({ asset: `${name}.webp`, out: `${r.dw}x${r.dh}`, status: 'PASS' })
}

// --- legs: centre-based placement (seeded from the socket, overridable) ---
const legPlacementOut = {}
function placeSegment(name, cx, cy, wf, rot) {
  const src = extract(BOX[name])
  const dw = Math.round(wf * CANVAS), dh = Math.round(dw * (src.h / src.w))
  const cxpx = cx * CANVAS, cypx = cy * CANVAS
  const png = blankCanvas()
  placeOnCanvas(src, dw, dh, cxpx, cypx, png, rot)
  saveWebp(`${name}.webp`, png)
  // hinge (top joint) canvas position = animation pivot for this segment
  const h = HINGE[name].top
  const pivot = mapPoint(src, dw, dh, cxpx, cypx, rot, h.x, h.y)
  manifest.layers[toKey(name)] = { file: `${name}.webp`, zIndex: Z[name], pivot: norm(pivot) }
  legPlacementOut[toKey(name)] = { wf, cx, cy, rot }
  report.push({ asset: `${name}.webp`, out: `${dw}x${dh}`, status: 'PASS' })
  return { src, dw, dh, cxpx, cypx, rot }
}
// derive a segment's centre from a socket target: place so a given tile-space
// hinge point lands on the socket (canvas px).
function centreFromSocket(name, hinge, wf, rot, socketPx) {
  const src = extract(BOX[name])
  const dw = Math.round(wf * CANVAS), dh = Math.round(dw * (src.h / src.w))
  const rad = rot * Math.PI / 180, cs = Math.cos(rad), sn = Math.sin(rad)
  const ox = (hinge.x - 0.5) * dw, oy = (hinge.y - 0.5) * dh
  return { cx: (socketPx.x - (ox * cs - oy * sn)) / CANVAS, cy: (socketPx.y - (ox * sn + oy * cs)) / CANVAS }
}
function placeLeg(kind) {
  const cfg = LEG[kind]
  const upName = `leg-${kind}-upper`, loName = `leg-${kind}-lower`
  const upKey = toKey(upName), loKey = toKey(loName)

  // upper: LEG_PLACE override, else socket-solved centre
  let up = LEG_PLACE[upKey]
  if (!up) {
    const c = centreFromSocket(upName, HINGE[upName].top, cfg.upper.wf, cfg.upper.rot,
      { x: cfg.socket.x * CANVAS, y: cfg.socket.y * CANVAS })
    up = { wf: cfg.upper.wf, cx: c.cx, cy: c.cy, rot: cfg.upper.rot }
  }
  const upR = placeSegment(upName, up.cx, up.cy, up.wf, up.rot)

  // lower: LEG_PLACE override, else attach its top-hinge to the upper's knee
  let lo = LEG_PLACE[loKey]
  if (!lo) {
    const knee = mapPoint(upR.src, upR.dw, upR.dh, upR.cxpx, upR.cypx, upR.rot, HINGE[upName].bottom.x, HINGE[upName].bottom.y)
    const c = centreFromSocket(loName, HINGE[loName].top, cfg.lower.wf, cfg.lower.rot, knee)
    lo = { wf: cfg.lower.wf, cx: c.cx, cy: c.cy, rot: cfg.lower.rot }
  }
  placeSegment(loName, lo.cx, lo.cy, lo.wf, lo.rot)

  manifest.legs[kind] = { upper: upKey, lower: loKey }
}
placeLeg('front')
placeLeg('hind')

// current placement (for the debug editor): torso/head/ear/tails + legs
manifest.placement = {}
for (const [name, P] of Object.entries(PLACE)) manifest.placement[toKey(name)] = { ...P }
Object.assign(manifest.placement, legPlacementOut)

// explicit pivot overrides (rotation origins) from the debug editor
for (const [key, p] of Object.entries(PIVOT)) {
  const k = toKey(key)
  if (manifest.layers[k]) manifest.layers[k].pivot = { x: p.x, y: p.y }
}

fs.writeFileSync(path.join(OUT, 'run.json'), JSON.stringify(manifest, null, 2) + '\n')

// ---------------------------------------------------------------------------
// reconstruction render (for visual iteration)
// ---------------------------------------------------------------------------
function checker(png, cell = 16) {
  for (let y = 0; y < png.height; y++) for (let x = 0; x < png.width; x++) {
    const c = (((x / cell) | 0) + ((y / cell) | 0)) & 1 ? 60 : 44
    const o = (y * png.width + x) * 4
    png.data[o] = c; png.data[o + 1] = c; png.data[o + 2] = c + 6; png.data[o + 3] = 255
  }
}
function over(png, layerPng) {
  for (let i = 0; i < png.width * png.height; i++) {
    const a = layerPng.data[i * 4 + 3] / 255
    if (!a) continue
    for (let k = 0; k < 3; k++)
      png.data[i * 4 + k] = layerPng.data[i * 4 + k] * a + png.data[i * 4 + k] * (1 - a)
  }
}
function loadWebp(rel) {
  const abs = path.join(OUT, rel)
  if (HAS_CWEBP) {
    const tmp = '/tmp/cfr-read.png'
    execFileSync('dwebp', ['-quiet', abs, '-o', tmp])
    return PNG.sync.read(fs.readFileSync(tmp))
  }
  return PNG.sync.read(fs.readFileSync(abs.replace(/\.webp$/, '.png')))
}
// far legs (tinted) then background tails, torso, near legs, head, ear
const ORDER = [
  'tail-blue', 'tail-orange',
  'leg-hind-upper', 'leg-hind-lower', 'leg-front-upper', 'leg-front-lower',
  'torso', 'head', 'ear',
].sort((a, b) => (Z[a] ?? 0) - (Z[b] ?? 0))
const recon = blankCanvas(); checker(recon)
for (const n of ORDER) over(recon, loadWebp(`${n}.webp`))
fs.writeFileSync(path.join(ROOT, 'cyber-fox-run-reconstruction.png'), PNG.sync.write(recon))

console.log('\n=== Cyber Fox gallop (side) rig build ===')
console.log('cwebp:', HAS_CWEBP ? 'yes (.webp)' : 'no (.png)')
console.log('out:', path.relative(ROOT, OUT))
console.table(report)
console.log('debug: cyber-fox-run-reconstruction.png')
