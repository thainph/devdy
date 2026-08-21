/**
 * DY — Cyber Fox renderer (Canvas 2D).
 *
 * A faithful 1:1 port of the layered-DOM/CSS mascot (CyberFox.vue) onto a single
 * <canvas> driven by ONE requestAnimationFrame loop. Every layer, keyframe and
 * effect from the CSS version is reproduced here so the visual output stays
 * identical, while collapsing dozens of composited DOM layers into one surface.
 *
 * Coordinate model: all fox layers are co-registered 1024² sprites. The DOM drew
 * each as `.l { inset:0; width/height:100% }` (fills the square box), so here we
 * simply drawImage(sprite, 0, 0, S, S). Manifest pivots are normalized [0,1] over
 * that same box → pivotPx = pivot * S. CSS keyframe translate percentages are
 * relative to the element box (S) → txPx = frac * S.
 */

import type { CyberFoxSize } from '@/composables/useMascotState'

export type FoxState =
  | 'idle'
  | 'thinking'
  | 'loading'
  | 'success'
  | 'error'
  | 'permission'
  | 'syncing'
  | 'sleep'

export type FoxRealm =
  | ''
  | 'luyen_khi'
  | 'truc_co'
  | 'kim_dan'
  | 'nguyen_anh'
  | 'hoa_than'
  | 'anh_bien'
  | 'van_dinh'

export interface FoxProps {
  state: FoxState
  size: CyberFoxSize | number
  streams: number
  lite: boolean
  reducedMotion: boolean
  evolutionRealm: FoxRealm
  evolutionTier: number
  speaking: boolean
}

const SIZES = { sm: 96, md: 148, lg: 196 } as const

// ---------------------------------------------------------------------------
// Assets (same glob + manifest the DOM component used).
// ---------------------------------------------------------------------------
const urlMap = import.meta.glob('../../fox-assets/generated/layers-2_5d/**/*.webp', {
  eager: true,
  query: '?url',
  import: 'default',
}) as Record<string, string>
const manifest = Object.values(
  import.meta.glob('../../fox-assets/generated/layers-2_5d/manifest.json', {
    eager: true,
    import: 'default',
  }),
)[0] as any

function assetUrl(rel: string): string {
  const hit = Object.keys(urlMap).find((k) => k.endsWith('/' + rel))
  return hit ? urlMap[hit] : ''
}

interface Pivot {
  x: number
  y: number
}
const PIVOT = {
  body: (manifest?.layers?.body?.pivot as Pivot) ?? { x: 0.5, y: 0.8867 },
  head: (manifest?.layers?.head?.pivot as Pivot) ?? { x: 0.4955, y: 0.5516 },
  earL: (manifest?.layers?.earLeft?.pivot as Pivot) ?? { x: 0.4063, y: 0.3099 },
  earR: (manifest?.layers?.earRight?.pivot as Pivot) ?? { x: 0.5968, y: 0.3045 },
  tailO: (manifest?.tails?.tailOrange?.pivot as Pivot) ?? { x: 0.4187, y: 0.6476 },
  tailB: (manifest?.tails?.tailBlue?.pivot as Pivot) ?? { x: 0.5006, y: 0.6787 },
}

// Sprite keys we load once as decoded bitmaps.
const SPRITE_SPECS: Record<string, string> = {
  body: 'body.webp',
  head: 'head.webp',
  earL: 'ear-left.webp',
  earR: 'ear-right.webp',
  tailO: 'tail-orange.webp',
  tailB: 'tail-blue.webp',
  chest: 'chest/idle.webp',
  eyeIdle: 'eyes/idle.webp',
  eyeBlink: 'eyes/blink.webp',
  eyeHappy: 'eyes/happy.webp',
  eyeError: 'eyes/error.webp',
  eyeP1: 'eyes/processing-01.webp',
  eyeP2: 'eyes/processing-02.webp',
  eyeP3: 'eyes/processing-03.webp',
}
for (let i = 0; i < 12; i++) SPRITE_SPECS['talk' + i] = `talk/${String(i).padStart(2, '0')}.webp`

type Sprites = Record<string, HTMLImageElement>

let sharedSprites: Sprites | null = null
let sharedReady: Promise<Sprites> | null = null

/** Load + decode every sprite once; shared across all renderer instances. */
export function loadFoxSprites(): Promise<Sprites> {
  if (sharedReady) return sharedReady
  sharedReady = (async () => {
    const entries = await Promise.all(
      Object.entries(SPRITE_SPECS).map(async ([key, rel]) => {
        const img = new Image()
        img.decoding = 'async'
        img.src = assetUrl(rel)
        try {
          await img.decode()
        } catch {
          await new Promise<void>((res) => {
            img.onload = () => res()
            img.onerror = () => res()
          })
        }
        return [key, img] as const
      }),
    )
    sharedSprites = Object.fromEntries(entries)
    return sharedSprites
  })()
  return sharedReady
}

// ---------------------------------------------------------------------------
// Evolution palette (mirrors the CSS custom-property blocks exactly).
// ---------------------------------------------------------------------------
interface RealmDef {
  rgb: [number, number, number]
  alt: [number, number, number]
  filter: string
}
const REALM: Record<Exclude<FoxRealm, ''>, RealmDef> = {
  luyen_khi: { rgb: [56, 189, 248], alt: [147, 197, 253], filter: 'hue-rotate(0deg) saturate(1.08)' },
  truc_co: { rgb: [16, 185, 129], alt: [110, 231, 183], filter: 'hue-rotate(58deg) saturate(1.25) brightness(1.04)' },
  kim_dan: { rgb: [245, 158, 11], alt: [252, 211, 77], filter: 'sepia(0.25) saturate(1.45) hue-rotate(340deg) brightness(1.08)' },
  nguyen_anh: { rgb: [139, 92, 246], alt: [196, 181, 253], filter: 'hue-rotate(175deg) saturate(1.35) brightness(1.06)' },
  hoa_than: { rgb: [244, 63, 94], alt: [251, 113, 133], filter: 'hue-rotate(310deg) saturate(1.4) brightness(1.08)' },
  anh_bien: { rgb: [6, 182, 212], alt: [103, 232, 249], filter: 'hue-rotate(105deg) saturate(1.35) brightness(1.06)' },
  van_dinh: { rgb: [217, 70, 239], alt: [250, 232, 255], filter: 'hue-rotate(230deg) saturate(1.45) brightness(1.1)' },
}
// Realm progression order — the level-up pillar accumulates one colour per realm
// reached, so higher-ranked foxes get a richer multi-colour beam.
const REALM_ORDER: Exclude<FoxRealm, ''>[] = [
  'luyen_khi', 'truc_co', 'kim_dan', 'nguyen_anh', 'hoa_than', 'anh_bien', 'van_dinh',
]
const TIER = {
  1: { power: 0.36, ring: 0.9, core: 0.044 },
  2: { power: 0.48, ring: 0.96, core: 0.05 },
  3: { power: 0.62, ring: 1.02, core: 0.056 },
  4: { power: 0.78, ring: 1.08, core: 0.062 },
  5: { power: 0.96, ring: 1.15, core: 0.07 },
} as const

// Orbit stream colours (radial gradient stops) — matches .dot--c0..c5.
const DOT_COLORS: Array<Array<[number, string]>> = [
  [[0.06, 'rgba(255,226,184,0.98)'], [0.38, 'rgba(255,168,80,0.6)'], [0.72, 'rgba(255,150,60,0)']],
  [[0.06, 'rgba(206,238,255,0.98)'], [0.38, 'rgba(112,190,255,0.6)'], [0.72, 'rgba(96,178,255,0)']],
  [[0.06, 'rgba(198,255,236,0.98)'], [0.38, 'rgba(80,230,180,0.6)'], [0.72, 'rgba(60,220,160,0)']],
  [[0.06, 'rgba(233,210,255,0.98)'], [0.38, 'rgba(180,130,255,0.6)'], [0.72, 'rgba(160,110,255,0)']],
  [[0.06, 'rgba(255,214,236,0.98)'], [0.38, 'rgba(255,120,190,0.6)'], [0.72, 'rgba(255,100,175,0)']],
  [[0.06, 'rgba(224,255,206,0.98)'], [0.38, 'rgba(160,235,90,0.6)'], [0.72, 'rgba(150,225,70,0)']],
]

// ---------------------------------------------------------------------------
// Math / easing helpers.
// ---------------------------------------------------------------------------
const TAU = Math.PI * 2
const DEG = Math.PI / 180
const clamp01 = (v: number) => (v < 0 ? 0 : v > 1 ? 1 : v)
const lerp = (a: number, b: number, u: number) => a + (b - a) * u
const wrap01 = (v: number) => ((v % 1) + 1) % 1

// easeInOut (≈ cubic-bezier(.42,0,.58,1)) — the default timing on the fox layers.
const easeInOut = (u: number) => (u < 0.5 ? 2 * u * u : 1 - Math.pow(-2 * u + 2, 2) / 2)
const linear = (u: number) => u

// Generic cubic-bezier(x1,y1,x2,y2) solver for the few non-standard curves.
function cubicBezier(x1: number, y1: number, x2: number, y2: number) {
  const cx = 3 * x1
  const bx = 3 * (x2 - x1) - cx
  const ax = 1 - cx - bx
  const cy = 3 * y1
  const by = 3 * (y2 - y1) - cy
  const ay = 1 - cy - by
  const fx = (t: number) => ((ax * t + bx) * t + cx) * t
  const fy = (t: number) => ((ay * t + by) * t + cy) * t
  return (x: number) => {
    let t = x
    for (let i = 0; i < 6; i++) {
      const dx = fx(t) - x
      if (Math.abs(dx) < 1e-4) break
      const d = (3 * ax * t + 2 * bx) * t + cx || 1e-6
      t -= dx / d
    }
    return fy(clamp01(t))
  }
}
const easeOutBack = cubicBezier(0.22, 1, 0.36, 1)

/** Piecewise keyframe interpolation. stops: [pos(0..1), value][]; eased per segment. */
function kf(stops: Array<[number, number]>, p: number, ease: (u: number) => number = easeInOut): number {
  for (let i = 0; i < stops.length - 1; i++) {
    const [p0, v0] = stops[i]
    const [p1, v1] = stops[i + 1]
    if (p <= p1) {
      const u = p1 === p0 ? 0 : (p - p0) / (p1 - p0)
      return lerp(v0, v1, ease(clamp01(u)))
    }
  }
  return stops[stops.length - 1][1]
}

// A single affine op (in CSS transform order) applied inside a pivot frame.
type Op = ['t', number, number] | ['r', number] | ['s', number, number]

// ---------------------------------------------------------------------------
// Renderer.
// ---------------------------------------------------------------------------
export class FoxRenderer {
  private ctx: CanvasRenderingContext2D
  private sprites: Sprites | null = null
  private raf = 0
  private running = false
  private startAt = 0
  private stateStartAt = 0
  private levelUpAt = 0
  private readonly LEVELUP_DUR = 1900
  private dpr = 1
  private cssSize = 148
  private supportsFilter: boolean

  private props: FoxProps = {
    state: 'idle',
    size: 'md',
    streams: 1,
    lite: false,
    reducedMotion: false,
    evolutionRealm: '',
    evolutionTier: 1,
    speaking: false,
  }

  constructor(private canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext('2d', { alpha: true })
    if (!ctx) throw new Error('2D canvas context unavailable')
    this.ctx = ctx
    // Feature-detect ctx.filter (used only for the evolution tint/glow).
    this.supportsFilter = typeof (ctx as any).filter === 'string'
  }

  async init() {
    this.sprites = await loadFoxSprites()
  }

  setProps(next: Partial<FoxProps>) {
    if (next.state && next.state !== this.props.state) {
      this.stateStartAt = performance.now()
    }
    this.props = { ...this.props, ...next }
    this.resize()
    // Draw one frame immediately so prop changes show even while paused.
    if (!this.running) this.drawOnce()
  }

  /** Fire the breakthrough VFX: glowing rings rise from the ground past the head. */
  triggerLevelUp() {
    this.levelUpAt = performance.now()
    if (!this.running) this.drawOnce()
  }

  /** True while the level-up rings are still animating (caller keeps the loop alive). */
  isLevelUpActive(): boolean {
    return this.levelUpAt > 0 && performance.now() - this.levelUpAt < this.LEVELUP_DUR
  }

  private numericSize(): number {
    const s = this.props.size
    return typeof s === 'number' ? Math.max(64, s) : SIZES[s]
  }

  private sizeBucket(): CyberFoxSize {
    const s = this.props.size
    if (typeof s !== 'number') return s
    return s < 120 ? 'sm' : s < 176 ? 'md' : 'lg'
  }

  resize() {
    const S = this.numericSize()
    this.cssSize = S
    this.dpr = Math.min(2, Math.max(1, window.devicePixelRatio || 1))
    const px = Math.round(S * this.dpr)
    if (this.canvas.width !== px || this.canvas.height !== px) {
      this.canvas.width = px
      this.canvas.height = px
    }
    this.canvas.style.width = `${S}px`
    this.canvas.style.height = `${S}px`
  }

  start() {
    if (this.running) return
    this.running = true
    const now = performance.now()
    if (!this.startAt) this.startAt = now
    if (!this.stateStartAt) this.stateStartAt = now
    const loop = () => {
      if (!this.running) return
      this.draw(performance.now())
      this.raf = requestAnimationFrame(loop)
    }
    this.raf = requestAnimationFrame(loop)
  }

  stop() {
    this.running = false
    if (this.raf) cancelAnimationFrame(this.raf)
    this.raf = 0
  }

  destroy() {
    this.stop()
  }

  private drawOnce() {
    if (!this.startAt) this.startAt = performance.now()
    this.draw(performance.now())
  }

  // Time helper: progress 0..1 through a `dur` ms loop, with optional delay ms.
  private cycle(now: number, dur: number, delay = 0): number {
    return wrap01((now - this.startAt - delay) / dur)
  }

  // -------------------------------------------------------------------------
  // Main frame.
  // -------------------------------------------------------------------------
  private draw(now: number) {
    if (!this.sprites) return
    const ctx = this.ctx
    const S = this.cssSize
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0)
    ctx.clearRect(0, 0, S, S)

    const p = this.props
    const evolved = !!p.evolutionRealm
    const tier = Math.max(1, Math.min(5, Math.round(p.evolutionTier || 1)))
    const bucket = this.sizeBucket()

    // Evolution decoration is intentionally limited to the concentric level
    // rings behind the fox (drawn in drawGroundRing) + the realm colour tint +
    // the chest core. The realm "mantle" outline and the stacked aura bands that
    // used to wrap the fox from foot to head were removed as visual clutter.
    this.drawGroundRing(now, S, evolved, tier)
    this.drawTails(now, S, evolved, tier)
    if (this.showOrbit() && bucket !== 'sm') this.drawOrbit(now, S, 'back')
    this.drawBody(now, S, evolved, tier)
    this.drawHeadGroup(now, S, evolved, tier, bucket)
    this.drawChest(now, S)
    if (this.showOrbit() && bucket !== 'sm') this.drawOrbit(now, S, 'front')
    this.drawLevelUpPillar(now, S)
  }

  // -------------------------------------------------------------------------
  // Level-up / breakthrough VFX: an RPG "pillar of light" rises from the ground
  // to envelop the fox, with a bright base flare and glowing motes drifting up
  // inside the beam. Drawn last, in 'lighter', so the fox bathes in the glow.
  // -------------------------------------------------------------------------
  private drawLevelUpPillar(now: number, S: number) {
    if (!this.levelUpAt || this.props.reducedMotion) return
    const elapsed = now - this.levelUpAt
    if (elapsed < 0 || elapsed > this.LEVELUP_DUR) {
      if (elapsed > this.LEVELUP_DUR) this.levelUpAt = 0
      return
    }

    const ctx = this.ctx
    const cx = 0.5 * S
    const groundY = 0.96 * S
    const prog = elapsed / this.LEVELUP_DUR

    // Accumulated palette: one colour per realm reached (warm gold as the base for
    // the very first realm), so the beam gains a colour with every rank-up.
    const idx = REALM_ORDER.indexOf(this.props.evolutionRealm as Exclude<FoxRealm, ''>)
    const palette: Array<[number, number, number]> = [[255, 216, 130]]
    if (idx >= 0) {
      for (let i = 0; i <= idx; i++) palette.push(REALM[REALM_ORDER[i]].rgb)
    }
    const n = palette.length
    // Sample the palette at a continuous position (wrapping), lerping neighbours.
    const sample = (t: number): [number, number, number] => {
      const f = ((t % n) + n) % n
      const i0 = Math.floor(f)
      const i1 = (i0 + 1) % n
      const k = f - i0
      const a = palette[i0], b = palette[i1]
      return [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k]
    }
    const rgbaOf = (c: [number, number, number], a: number) =>
      `rgba(${Math.round(c[0])},${Math.round(c[1])},${Math.round(c[2])},${clamp01(a).toFixed(3)})`
    const W = (a: number) => `rgba(255,255,255,${clamp01(a).toFixed(3)})`

    // Overall intensity envelope + the beam's rising reveal from the ground.
    const env = kf([[0, 0], [0.14, 1], [0.66, 1], [1, 0]], prog)
    const r = clamp01(prog / 0.26)
    const rise = 1 - Math.pow(1 - r, 3) // easeOutCubic
    const topY = lerp(groundY, -0.28 * S, rise) // runs off-canvas at full height
    const shimmer = 0.82 + 0.18 * Math.sin(elapsed * 0.018)
    const scroll = elapsed / 520 // colours flow upward over time
    const pseudo = (m: number) => {
      const x = Math.sin(m * 127.1 + 311.7) * 43758.5453
      return x - Math.floor(x)
    }

    ctx.save()
    ctx.globalCompositeOperation = 'lighter'

    // 1) Wide soft beam, rendered as horizontal-feathered slices whose colour is
    //    sampled from the palette per height → a multi-colour gradient flowing up.
    const halfW = 0.25 * S
    const SLICES = this.props.lite ? 14 : 26
    const beamH = groundY - topY
    for (let s = 0; s < SLICES; s++) {
      const f = s / (SLICES - 1) // 0 at ground → 1 at top
      const y0 = groundY - (beamH * s) / SLICES
      const y1 = groundY - (beamH * (s + 1)) / SLICES
      const col = sample(f * (n + 0.5) - scroll)
      const a = 0.24 * env * shimmer * (0.5 + 0.5 * (1 - f)) // a touch brighter low
      const g = ctx.createLinearGradient(cx - halfW, 0, cx + halfW, 0)
      g.addColorStop(0, rgbaOf(col, 0))
      g.addColorStop(0.5, rgbaOf(col, a))
      g.addColorStop(1, rgbaOf(col, 0))
      ctx.fillStyle = g
      ctx.fillRect(cx - halfW, y1, halfW * 2, y0 - y1 + 0.5)
    }

    // 2) Bright white core column keeps the beam crisp regardless of hue.
    const coreW = 0.055 * S
    const core = ctx.createLinearGradient(cx - coreW, 0, cx + coreW, 0)
    core.addColorStop(0, W(0))
    core.addColorStop(0.5, W(0.38 * env * shimmer))
    core.addColorStop(1, W(0))
    ctx.fillStyle = core
    ctx.fillRect(cx - coreW, topY, coreW * 2, groundY - topY)

    // 3) Ground flare — flattened radial burst that flashes hard at the start,
    //    tinted with the fox's current (highest) realm colour.
    const flash = kf([[0, 0], [0.1, 1], [0.4, 0.4], [1, 0]], prog)
    const baseCol = palette[n - 1]
    const fr = 0.36 * S
    ctx.save()
    ctx.translate(cx, groundY)
    ctx.scale(1, 0.42)
    const flare = ctx.createRadialGradient(0, 0, 0, 0, 0, fr)
    flare.addColorStop(0, W(0.65 * flash))
    flare.addColorStop(0.4, rgbaOf(baseCol, 0.55 * flash))
    flare.addColorStop(1, rgbaOf(baseCol, 0))
    ctx.fillStyle = flare
    ctx.beginPath()
    ctx.arc(0, 0, fr, 0, TAU)
    ctx.fill()
    ctx.restore()

    // 4) Glowing motes drifting up, each carrying one of the palette colours.
    const N = this.props.lite ? 9 : 16
    for (let i = 0; i < N; i++) {
      const off = pseudo(i)
      const spd = 0.85 + pseudo(i + 50) * 0.6
      const local = (prog * spd + off) % 1
      const pa = Math.sin(Math.PI * local)
      if (pa <= 0.02) continue
      const col = palette[i % n]
      const py = lerp(groundY - 0.02 * S, -0.02 * S, local)
      const spread = (pseudo(i + 9) - 0.5) * 0.34 * S
      const wig = Math.sin(local * TAU * 2 + i) * 0.02 * S
      const px = cx + spread * (1 - local * 0.35) + wig
      const size = (0.007 + pseudo(i + 3) * 0.008) * S * (0.6 + 0.4 * pa)
      const pg = ctx.createRadialGradient(px, py, 0, px, py, size)
      pg.addColorStop(0, W(pa * env))
      pg.addColorStop(0.5, rgbaOf(col, pa * env * 0.85))
      pg.addColorStop(1, rgbaOf(col, 0))
      ctx.fillStyle = pg
      ctx.beginPath()
      ctx.arc(px, py, size, 0, TAU)
      ctx.fill()
    }

    ctx.restore()
  }

  private showOrbit(): boolean {
    const p = this.props
    if (p.reducedMotion) return false
    return p.state === 'loading' || p.state === 'syncing'
  }

  private ePower(tier: number) {
    return TIER[tier as 1].power
  }

  // -------------------------------------------------------------------------
  // Layer transform helper.
  // -------------------------------------------------------------------------
  private applyPivot(pivot: Pivot, S: number, ops: Op[]) {
    const ctx = this.ctx
    const px = pivot.x * S
    const py = pivot.y * S
    ctx.translate(px, py)
    for (const op of ops) {
      if (op[0] === 't') ctx.translate(op[1] * S, op[2] * S)
      else if (op[0] === 'r') ctx.rotate(op[1] * DEG)
      else ctx.scale(op[1], op[2])
    }
    ctx.translate(-px, -py)
  }

  private full(img: HTMLImageElement, S: number) {
    if (img && img.width) this.ctx.drawImage(img, 0, 0, S, S)
  }

  // -------------------------------------------------------------------------
  // Body + chest breathing (shared driver, body pivot).
  // -------------------------------------------------------------------------
  /** Vertical lift (fraction of S) for the breakthrough double-hop, or null when idle. */
  private levelUpHop(now: number): number | null {
    if (!this.levelUpAt || this.props.reducedMotion) return null
    const el = now - this.levelUpAt
    if (el < 0 || el >= 1300) return null
    const c = el / 1300
    return kf(
      [[0, 0], [0.18, -0.12], [0.4, 0], [0.58, -0.06], [0.78, 0], [1, 0]],
      c,
      easeOutBack,
    )
  }

  private breatheOps(now: number): Op[] {
    const p = this.props
    if (p.reducedMotion) return []
    // Breakthrough celebration: a lively double hop, overriding normal motion
    // while the level-up VFX plays. Translate-only so the head (headOps) can
    // mirror the exact same lift and stay glued to the body.
    const hop = this.levelUpHop(now)
    if (hop !== null) return [['t', 0, hop]]
    if (p.state === 'success') {
      // dy-success 1.35s: translate + scale.
      const c = this.cycle(now, 1350)
      const ty = kf([[0, 0], [0.25, -0.04], [0.55, 0.005], [1, 0]], c, easeOutBack)
      const sc = kf([[0, 1], [0.25, 1.03], [0.55, 0.995], [1, 1]], c, easeOutBack)
      return [['t', 0, ty], ['s', sc, sc]]
    }
    const dur = p.state === 'sleep' ? 4600 : 3000
    const c = this.cycle(now, dur)
    // dy-breathe: translateY 0→-1.5%→0 ; scaleY 1→1.006→1
    const ty = kf([[0, 0], [0.5, -0.015], [1, 0]], c)
    const sy = kf([[0, 1], [0.5, 1.006], [1, 1]], c)
    return [['t', 0, ty], ['s', 1, sy]]
  }

  private drawBody(now: number, S: number, evolved: boolean, tier: number) {
    const ctx = this.ctx
    ctx.save()
    this.applyPivot(PIVOT.body, S, this.breatheOps(now))
    if (evolved && this.supportsFilter) ctx.filter = this.layerFilter(tier, S)
    this.full(this.sprites!.body, S)
    ctx.filter = 'none'
    ctx.restore()
  }

  private layerFilter(tier: number, S: number): string {
    const realm = REALM[this.props.evolutionRealm as Exclude<FoxRealm, ''>]
    if (!realm) return 'none'
    const power = this.ePower(tier)
    const [r, g, b] = realm.rgb
    const a = (0.12 + power * 0.18).toFixed(3)
    const blur = (S * 0.018).toFixed(2)
    return `${realm.filter} saturate(${(1 + power * 0.18).toFixed(3)}) drop-shadow(0 0 ${blur}px rgba(${r},${g},${b},${a}))`
  }

  // -------------------------------------------------------------------------
  // Head group (head + ears + eyes share the head transform).
  // -------------------------------------------------------------------------
  private headOps(now: number): Op[] {
    const p = this.props
    if (p.reducedMotion) return []
    // During the breakthrough hop the head rides the exact body lift, plus a
    // small happy wobble, so it stays locked to the body while jumping.
    const hop = this.levelUpHop(now)
    if (hop !== null) {
      const el = now - this.levelUpAt
      const rot = kf([[0, 0], [0.2, -3], [0.45, 3], [0.7, -1.5], [1, 0]], el / 1300)
      return [['t', 0, hop], ['r', rot]]
    }
    switch (p.state) {
      case 'thinking': {
        const c = this.cycle(now, 2800)
        // dy-head-ponder: rotate(-9→9) translate(-1.5%→1.5%, 0→-1%)
        const rot = kf([[0, -9], [0.5, 9], [1, -9]], c)
        const tx = kf([[0, -0.015], [0.5, 0.015], [1, -0.015]], c)
        const ty = kf([[0, 0], [0.5, -0.01], [1, 0]], c)
        return [['r', rot], ['t', tx, ty]]
      }
      case 'success': {
        const c = this.cycle(now, 1000)
        // dy-head-nod
        const ty = kf([[0, 0], [0.3, -0.045], [0.6, 0.01], [1, 0]], c)
        const rot = kf([[0, 0], [0.3, 0.5], [0.6, -0.5], [1, 0]], c)
        return [['t', 0, ty], ['r', rot]]
      }
      case 'error': {
        const c = this.cycle(now, 800)
        const rot = kf([[0, 0], [0.18, -5], [0.38, 4.5], [0.58, -3.5], [0.78, 2.5], [1, 0]], c)
        return [['r', rot]]
      }
      case 'permission': {
        const c = this.cycle(now, 1800)
        const rot = kf([[0, -5], [0.5, -3], [1, -5]], c)
        const ty = kf([[0, 0], [0.5, -0.01], [1, 0]], c)
        return [['r', rot], ['t', 0, ty]]
      }
      case 'sleep': {
        const c = this.cycle(now, 4600)
        const ty = kf([[0, 0.015], [0.5, 0.03], [1, 0.015]], c)
        const rot = kf([[0, -2], [0.5, -3], [1, -2]], c)
        return [['t', 0, ty], ['r', rot]]
      }
      default: {
        // dy-head (idle/loading/syncing)
        const c = this.cycle(now, 3000)
        const ty = kf([[0, 0], [0.3, -0.01], [0.65, -0.004], [1, 0]], c)
        const rot = kf([[0, 0], [0.3, -1], [0.65, 1], [1, 0]], c)
        return [['t', 0, ty], ['r', rot]]
      }
    }
  }

  private earOps(now: number, side: 'L' | 'R', bucket: CyberFoxSize): Op[] {
    const p = this.props
    if (p.reducedMotion || bucket === 'sm') return []
    const c = (dur: number) => this.cycle(now, dur)
    const perk = (base: number, peak: number, dur: number): Op[] => {
      const v = kf([[0, base], [0.5, peak], [1, base]], c(dur))
      return [['r', v]]
    }
    switch (p.state) {
      case 'thinking':
        return side === 'L' ? perk(-3, -7.5, 1500) : perk(3, 7.5, 1700)
      case 'success':
        return side === 'L' ? perk(-3, -7.5, 850) : perk(3, 7.5, 920)
      case 'permission':
        return side === 'L' ? perk(-3, -7.5, 1200) : perk(3, 7.5, 1280)
      case 'error':
        return side === 'L' ? perk(6, 9, 2000) : perk(-6, -9, 2150)
      case 'sleep':
        return side === 'L' ? perk(6, 9, 4400) : perk(-6, -9, 4700)
      default: {
        // dy-ear-l / dy-ear-r (idle/loading/syncing)
        if (side === 'L') {
          const v = kf([[0, 0], [0.2, 0], [0.33, -4], [0.42, -1.2], [0.5, -2.6], [0.58, 0], [1, 0]], c(3000))
          return [['r', v]]
        }
        const v = kf([[0, 0], [0.24, 0], [0.38, 3.6], [0.47, 1], [0.55, 2.2], [0.62, 0], [1, 0]], c(3000))
        return [['r', v]]
      }
    }
  }

  private drawHeadGroup(now: number, S: number, evolved: boolean, tier: number, bucket: CyberFoxSize) {
    const ctx = this.ctx
    const sp = this.sprites!
    ctx.save()
    this.applyPivot(PIVOT.head, S, this.headOps(now))

    // Head image (swap to a talking mouth-flap sprite while speaking).
    const talk = this.props.speaking && !this.props.reducedMotion
    let headImg = sp.head
    if (talk) {
      const TALK_SEQ = [0, 1, 2, 3, 2, 1]
      const step = Math.floor((now - this.startAt) / 110) % TALK_SEQ.length
      headImg = sp['talk' + TALK_SEQ[step]] || sp.talk0 || sp.head
    }
    if (evolved && this.supportsFilter) ctx.filter = this.layerFilter(tier, S)
    this.full(headImg, S)
    ctx.filter = 'none'

    // Ears (nested own transform, evolution tint).
    ctx.save()
    this.applyPivot(PIVOT.earL, S, this.earOps(now, 'L', bucket))
    if (evolved && this.supportsFilter) ctx.filter = this.layerFilter(tier, S)
    this.full(sp.earL, S)
    ctx.filter = 'none'
    ctx.restore()

    ctx.save()
    this.applyPivot(PIVOT.earR, S, this.earOps(now, 'R', bucket))
    if (evolved && this.supportsFilter) ctx.filter = this.layerFilter(tier, S)
    this.full(sp.earR, S)
    ctx.filter = 'none'
    ctx.restore()

    // Eyes (no own transform; alpha/crossfade + evolution glow).
    this.drawEyes(now, S, evolved, tier)

    ctx.restore()
  }

  private drawEyes(now: number, S: number, evolved: boolean, tier: number) {
    const ctx = this.ctx
    const sp = this.sprites!
    const p = this.props
    const processingStates = p.state === 'loading' || p.state === 'thinking' || p.state === 'syncing'
    const showProcessing = processingStates && !p.reducedMotion

    if (evolved && this.supportsFilter) {
      const realm = REALM[p.evolutionRealm as Exclude<FoxRealm, ''>]
      if (realm) {
        const power = this.ePower(tier)
        const [r, g, b] = realm.alt
        const a = (0.42 + power * 0.38).toFixed(3)
        ctx.filter = `drop-shadow(0 0 ${(S * 0.025).toFixed(2)}px rgba(${r},${g},${b},${a}))`
      }
    }

    if (showProcessing) {
      const imgs = [sp.eyeP1, sp.eyeP2, sp.eyeP3]
      if (p.state === 'loading') {
        // dy-eye-cycle: hard 1/3 switch (steps).
        const c = this.cycle(now, 1200)
        const idx = c < 1 / 3 ? 0 : c < 2 / 3 ? 1 : 2
        // three layers with -1/3, -2/3 phase → net: exactly one visible.
        this.full(imgs[idx], S)
      } else {
        // dy-eye-seq crossfade, 3 layers phase-shifted by 1/3.
        const dur = 1200
        const seqAlpha = (c: number) => kf([[0, 1], [0.22, 1], [0.4, 0], [0.88, 0], [1, 1]], c)
        for (let i = 0; i < 3; i++) {
          const c = this.cycle(now, dur, -(dur / 3) * i)
          ctx.globalAlpha = clamp01(seqAlpha(c))
          this.full(imgs[i], S)
        }
        ctx.globalAlpha = 1
      }
    } else {
      let eye = sp.eyeIdle
      if (p.state === 'success') eye = sp.eyeHappy
      else if (p.state === 'error') eye = sp.eyeError
      else if (p.state === 'sleep') eye = sp.eyeBlink
      this.full(eye, S)
      // Blink overlay for non-error/sleep/success states.
      const showBlink = !['error', 'sleep', 'success'].includes(p.state) && !p.reducedMotion
      if (showBlink) {
        const c = this.cycle(now, 6000)
        const a = kf([[0, 0], [0.95, 0], [0.965, 0.9], [0.98, 0.9], [1, 0]], c, linear)
        if (a > 0.001) {
          ctx.globalAlpha = a
          this.full(sp.eyeBlink, S)
          ctx.globalAlpha = 1
        }
      }
    }
    ctx.filter = 'none'
  }

  // -------------------------------------------------------------------------
  // Tails (base + rhythmic glow overlay).
  // -------------------------------------------------------------------------
  private tailParams(): { oDur: number; bDur: number; tw: number } {
    switch (this.props.state) {
      case 'success':
        return { oDur: 900, bDur: 960, tw: 1.7 }
      case 'error':
        return { oDur: 3200, bDur: 3400, tw: 0.5 }
      case 'syncing':
        return { oDur: 1400, bDur: 1520, tw: 1.2 }
      case 'sleep':
        return { oDur: 4200, bDur: 4400, tw: 0.6 }
      case 'thinking':
        return { oDur: 2600, bDur: 2800, tw: 1.15 }
      default:
        return { oDur: 2600, bDur: 2800, tw: 1 }
    }
  }

  private drawTails(now: number, S: number, evolved: boolean, tier: number) {
    const ctx = this.ctx
    const sp = this.sprites!
    const p = this.props
    const { oDur, bDur, tw } = this.tailParams()
    const reduced = p.reducedMotion

    // Ride the breakthrough hop so the tails stay attached while the fox jumps.
    const hop = this.levelUpHop(now)
    const hopOp: Op[] = hop !== null ? [['t', 0, hop]] : []
    // dy-tail-o: rotate(-3*tw→6*tw) scale(1→1.03) ; dy-tail-b: rotate(3*tw→-5.5*tw)
    const oOps = (): Op[] => {
      if (reduced) return hopOp
      const c = this.cycle(now, oDur)
      const rot = kf([[0, -3 * tw], [0.5, 6 * tw], [1, -3 * tw]], c)
      const sc = kf([[0, 1], [0.5, 1.03], [1, 1]], c)
      return [...hopOp, ['r', rot], ['s', sc, sc]]
    }
    const bOps = (): Op[] => {
      if (reduced) return hopOp
      const c = this.cycle(now, bDur)
      const rot = kf([[0, 3 * tw], [0.5, -5.5 * tw], [1, 3 * tw]], c)
      const sc = kf([[0, 1], [0.5, 1.03], [1, 1]], c)
      return [...hopOp, ['r', rot], ['s', sc, sc]]
    }
    // dy-glow opacity: .28→.85(45%)→.5(70%)→.28
    const glowAlpha = (dur: number, delay: number) => {
      const c = this.cycle(now, dur, delay)
      return kf([[0, 0.28], [0.45, 0.85], [0.7, 0.5], [1, 0.28]], c)
    }

    const drawTail = (img: HTMLImageElement, pivot: Pivot, ops: Op[], glowDur: number, glowDelay: number) => {
      // base
      ctx.save()
      this.applyPivot(pivot, S, ops)
      if (evolved && this.supportsFilter) ctx.filter = this.layerFilter(tier, S)
      this.full(img, S)
      ctx.filter = 'none'
      ctx.restore()
      // glow overlay (skipped in lite / reduced)
      if (p.lite || reduced) return
      ctx.save()
      this.applyPivot(pivot, S, ops)
      ctx.globalAlpha = clamp01(glowAlpha(glowDur, glowDelay))
      if (evolved && this.supportsFilter) {
        const realm = REALM[p.evolutionRealm as Exclude<FoxRealm, ''>]
        if (realm) {
          const power = this.ePower(tier)
          const [r, g, b] = realm.rgb
          const a = (0.24 + power * 0.36).toFixed(3)
          ctx.filter = `saturate(${(1.1 + power * 0.22).toFixed(3)}) drop-shadow(0 0 ${(S * 0.05).toFixed(2)}px rgba(${r},${g},${b},${a}))`
        }
      }
      this.full(img, S)
      ctx.filter = 'none'
      ctx.globalAlpha = 1
      ctx.restore()
    }

    // Source order: blue (z20) then orange (z22).
    drawTail(sp.tailB, PIVOT.tailB, bOps(), bDur, -160)
    drawTail(sp.tailO, PIVOT.tailO, oOps(), oDur, -140)
  }

  // -------------------------------------------------------------------------
  // Orbiting light streams (motion-path port).
  // -------------------------------------------------------------------------
  private orbits(S: number) {
    const lite = this.props.lite
    const MAX = lite ? 3 : 6
    const n = Math.max(1, Math.min(MAX, Math.round(this.props.streams || 1)))
    const cx = 0.5 * S
    const cy = 0.52 * S
    const list = []
    for (let i = 0; i < n; i++) {
      const tiltDeg = -34 + (i * 180) / n
      const rx = (0.4 + (i % 3) * 0.017) * S
      const ry = (0.16 + ((i + 1) % 3) * 0.022) * S
      list.push({
        tilt: tiltDeg * DEG,
        rx,
        ry,
        cx,
        cy,
        color: i % 6,
        phase: n > 1 ? i / n : 0,
        dur: 3000 + i * 280,
      })
    }
    return list
  }

  private ellipsePoint(o: { tilt: number; rx: number; ry: number; cx: number; cy: number }, f: number) {
    const a = -Math.PI / 2 + f * TAU
    const x = o.rx * Math.cos(a)
    const y = o.ry * Math.sin(a)
    const cos = Math.cos(o.tilt)
    const sin = Math.sin(o.tilt)
    return { x: x * cos - y * sin + o.cx, y: x * sin + y * cos + o.cy }
  }

  private drawOrbit(now: number, S: number, pass: 'back' | 'front') {
    const ctx = this.ctx
    const lite = this.props.lite
    const TRAIL = lite ? 6 : 9
    const TRAIL_STEP = 0.028
    ctx.save()
    ctx.globalCompositeOperation = lite ? 'source-over' : 'lighter'
    for (const o of this.orbits(S)) {
      for (let i = 1; i <= TRAIL; i++) {
        const frac = wrap01(o.phase - (i - 1) * TRAIL_STEP)
        const prog = wrap01((now - this.startAt) / o.dur + frac)
        // dy-orbit-z: front during 25%..75% of the path.
        const front = prog >= 0.25 && prog < 0.75
        if ((pass === 'front') !== front) continue
        const k = (i - 1) / (TRAIL - 1)
        const sVar = 1 - k * 0.58
        const oVar = 1 - k * 0.72
        const pt = this.ellipsePoint(o, prog)
        const r = 0.055 * S * sVar // width 11% → radius 5.5%
        const grad = ctx.createRadialGradient(pt.x, pt.y, 0, pt.x, pt.y, r)
        for (const [stop, col] of DOT_COLORS[o.color]) grad.addColorStop(clamp01(stop / 0.72), col)
        ctx.globalAlpha = clamp01(oVar)
        ctx.fillStyle = grad
        ctx.beginPath()
        ctx.arc(pt.x, pt.y, r, 0, TAU)
        ctx.fill()
      }
    }
    ctx.globalAlpha = 1
    ctx.globalCompositeOperation = 'source-over'
    ctx.restore()
  }

  // -------------------------------------------------------------------------
  // Chest terminal + state indicators.
  // -------------------------------------------------------------------------
  private drawChest(now: number, S: number) {
    const ctx = this.ctx
    const p = this.props
    ctx.save()
    this.applyPivot(PIVOT.body, S, this.breatheOps(now))

    const cx = 0.499 * S
    const cy = 0.671 * S

    if (p.state === 'thinking') this.chestThink(now, S, cx, cy)
    else if (p.state === 'loading') this.chestLoad(now, S, cx, cy)
    else if (p.state === 'success') this.chestOk(now, S, cx, cy)
    else if (p.state === 'error') this.chestErr(now, S, cx, cy)
    else if (p.state === 'permission') this.chestAsk(now, S, cx, cy)
    else {
      // idle / sleep / syncing → chest image with dy-chest scale.
      const dur = p.state === 'idle' || p.state === 'sleep' ? 2400 : 1200
      const c = p.reducedMotion ? 0 : this.cycle(now, dur)
      const sc = p.reducedMotion ? 1 : kf([[0, 1], [0.5, 1.035], [1, 1]], c)
      ctx.save()
      ctx.translate(0.5 * S, 0.68 * S)
      ctx.scale(sc, sc)
      ctx.translate(-0.5 * S, -0.68 * S)
      this.full(this.sprites!.chest, S)
      ctx.restore()
    }
    ctx.restore()
  }

  private chestPanel(S: number, cx: number, cy: number, glow: string) {
    const ctx = this.ctx
    const w = 0.15 * S
    const h = w / 1.42
    const x = cx - w / 2
    const y = cy - h / 2
    const r = w * 0.22

    // Outer soft glow + metallic bezel frame.
    ctx.save()
    ctx.shadowColor = glow
    ctx.shadowBlur = S * 0.055
    this.roundRect(x, y, w, h, r)
    const bezel = ctx.createLinearGradient(x, y, x, y + h)
    bezel.addColorStop(0, '#2a4370')
    bezel.addColorStop(0.5, '#152944')
    bezel.addColorStop(1, '#0a1428')
    ctx.fillStyle = bezel
    ctx.fill()
    ctx.restore()

    // Recessed screen inset into the bezel.
    const inset = w * 0.06
    const ix = x + inset
    const iy = y + inset
    const iw = w - inset * 2
    const ih = h - inset * 2
    const ir = r * 0.68
    ctx.save()
    this.roundRect(ix, iy, iw, ih, ir)
    const g = ctx.createRadialGradient(cx, iy + ih * 0.35, 0, cx, iy + ih * 0.35, iw * 0.72)
    g.addColorStop(0, '#0e1f45')
    g.addColorStop(1, '#04081a')
    ctx.fillStyle = g
    ctx.fill()
    // Glassy top highlight across the screen.
    ctx.clip()
    const hl = ctx.createLinearGradient(0, iy, 0, iy + ih * 0.55)
    hl.addColorStop(0, 'rgba(150,200,255,0.16)')
    hl.addColorStop(1, 'rgba(150,200,255,0)')
    ctx.fillStyle = hl
    ctx.fillRect(ix, iy, iw, ih * 0.55)
    ctx.restore()

    return { x, y, w, h }
  }

  private roundRect(x: number, y: number, w: number, h: number, r: number) {
    const ctx = this.ctx
    ctx.beginPath()
    ctx.moveTo(x + r, y)
    ctx.arcTo(x + w, y, x + w, y + h, r)
    ctx.arcTo(x + w, y + h, x, y + h, r)
    ctx.arcTo(x, y + h, x, y, r)
    ctx.arcTo(x, y, x + w, y, r)
    ctx.closePath()
  }

  private chestThink(now: number, S: number, cx: number, cy: number) {
    const ctx = this.ctx
    const { w, h } = this.chestPanel(S, cx, cy, 'rgba(90,160,255,0.42)')
    const dot = w * 0.155
    const gap = w * 0.085
    const totalW = dot * 3 + gap * 2
    let dx = cx - totalW / 2 + dot / 2
    const baseY = cy + h * 0.1
    const delays = [0, 0.16 / 1.3, 0.32 / 1.3]
    for (let i = 0; i < 3; i++) {
      const c = this.props.reducedMotion ? 0.3 : this.cycle(now, 1300, -delays[i] * 1300)
      const lift = kf([[0, 0], [0.3, -0.32], [0.6, 0], [1, 0]], c)
      const ty = lift * h
      const sc = kf([[0, 0.82], [0.3, 1.14], [0.6, 0.82], [1, 0.82]], c)
      const op = kf([[0, 0.4], [0.3, 1], [0.6, 0.4], [1, 0.4]], c)
      const rr = (dot / 2) * sc

      // Faint reflection puddle on the baseline — fades as the dot lifts.
      ctx.save()
      ctx.globalAlpha = 0.22 * (1 + lift * 2)
      ctx.fillStyle = '#3f97ff'
      ctx.beginPath()
      ctx.ellipse(dx, baseY, rr * 0.85, rr * 0.32, 0, 0, TAU)
      ctx.fill()
      ctx.restore()

      // Glossy dot with an off-centre highlight.
      const cyD = baseY + ty
      ctx.save()
      ctx.globalAlpha = op
      const g = ctx.createRadialGradient(dx - rr * 0.32, cyD - rr * 0.36, rr * 0.1, dx, cyD, rr)
      g.addColorStop(0, '#ffffff')
      g.addColorStop(0.32, '#d3ecff')
      g.addColorStop(1, '#3f97ff')
      ctx.fillStyle = g
      ctx.shadowColor = 'rgba(120,190,255,0.9)'
      ctx.shadowBlur = S * 0.022
      ctx.beginPath()
      ctx.arc(dx, cyD, rr, 0, TAU)
      ctx.fill()
      ctx.restore()
      dx += dot + gap
    }
  }

  private chestLoad(now: number, S: number, cx: number, cy: number) {
    const ctx = this.ctx
    const { x, w, h } = this.chestPanel(S, cx, cy, 'rgba(90,160,255,0.42)')
    const trackX = x + w * 0.17
    const trackW = w * 0.66
    const barH = h * 0.17
    const by = cy - barH / 2
    const trackR = barH / 2

    // Recessed progress channel.
    ctx.save()
    this.roundRect(trackX, by, trackW, barH, trackR)
    ctx.fillStyle = 'rgba(80,140,220,0.16)'
    ctx.fill()
    ctx.restore()

    // Indeterminate glowing segment sweeping across the channel.
    ctx.save()
    this.roundRect(trackX, by, trackW, barH, trackR)
    ctx.clip()
    const segW = trackW * 0.5
    const c = this.props.reducedMotion ? 0.5 : this.cycle(now, 1150)
    const bx = lerp(-segW, trackW, c) + trackX
    const g = ctx.createLinearGradient(bx, 0, bx + segW, 0)
    g.addColorStop(0, 'rgba(120,190,255,0)')
    g.addColorStop(0.5, '#7cc0ff')
    g.addColorStop(1, 'rgba(120,190,255,0)')
    ctx.fillStyle = g
    ctx.shadowColor = 'rgba(120,190,255,0.85)'
    ctx.shadowBlur = S * 0.02
    ctx.fillRect(bx, by, segW, barH)
    ctx.restore()
  }

  private chestOk(now: number, S: number, cx: number, cy: number) {
    const ctx = this.ctx
    this.chestPanel(S, cx, cy, 'rgba(70,220,150,0.42)')
    const since = now - this.stateStartAt
    const c = this.props.reducedMotion ? 1 : clamp01(since / 500)
    const sc = kf([[0, 0.5], [0.6, 1.15], [1, 1]], c, easeOutBack)
    const op = kf([[0, 0], [0.6, 1], [1, 1]], c)
    ctx.save()
    ctx.globalAlpha = op
    ctx.translate(cx, cy)
    ctx.scale(sc, sc)
    const u = 0.15 * S * 0.48
    ctx.strokeStyle = '#5be6a6'
    ctx.lineWidth = S * 0.012
    ctx.lineCap = 'round'
    ctx.lineJoin = 'round'
    ctx.shadowColor = 'rgba(70,220,150,0.9)'
    ctx.shadowBlur = S * 0.012
    ctx.beginPath()
    // check: M5 13 l4 4 L19 7 within 24-box, centered
    const map = (px: number, py: number) => [((px - 12) / 24) * u * 2, ((py - 12) / 24) * u * 2]
    let [mx, my] = map(5, 13)
    ctx.moveTo(mx, my)
    ;[mx, my] = map(9, 17)
    ctx.lineTo(mx, my)
    ;[mx, my] = map(19, 7)
    ctx.lineTo(mx, my)
    ctx.stroke()
    ctx.restore()
  }

  private chestErr(now: number, S: number, cx: number, cy: number) {
    const ctx = this.ctx
    this.chestPanel(S, cx, cy, 'rgba(255,90,90,0.42)')
    const since = now - this.stateStartAt
    const c = this.props.reducedMotion ? 1 : clamp01(since / 500)
    const shake = this.props.reducedMotion ? 0 : kf([[0, 0], [0.2, -0.12], [0.4, 0.1], [0.6, -0.07], [0.8, 0.04], [1, 0]], c)
    const rot = this.props.reducedMotion ? 0 : kf([[0, 0], [0.2, -4], [0.4, 3], [0.6, -2], [0.8, 0], [1, 0]], c)
    // Same viewBox mapping as the check so both glyphs read at the same scale.
    const u = 0.15 * S * 0.46
    const map = (px: number, py: number) => [((px - 12) / 24) * u * 2, ((py - 12) / 24) * u * 2]
    ctx.save()
    ctx.translate(cx + shake * 0.15 * S, cy)
    ctx.rotate(rot * DEG)
    ctx.strokeStyle = '#ff6b6b'
    ctx.lineWidth = S * 0.012
    ctx.lineCap = 'round'
    ctx.lineJoin = 'round'
    ctx.shadowColor = 'rgba(255,90,90,0.9)'
    ctx.shadowBlur = S * 0.012
    ctx.beginPath()
    // cross: M6 6 L18 18 ; M18 6 L6 18 within the 24-box, centered
    let [mx, my] = map(6, 6)
    ctx.moveTo(mx, my)
    ;[mx, my] = map(18, 18)
    ctx.lineTo(mx, my)
    ;[mx, my] = map(18, 6)
    ctx.moveTo(mx, my)
    ;[mx, my] = map(6, 18)
    ctx.lineTo(mx, my)
    ctx.stroke()
    ctx.restore()
  }

  private chestAsk(now: number, S: number, cx: number, cy: number) {
    const ctx = this.ctx
    this.chestPanel(S, cx, cy, 'rgba(255,170,60,0.42)')
    const c = this.props.reducedMotion ? 0.5 : this.cycle(now, 1500)
    const sc = kf([[0, 0.92], [0.5, 1.08], [1, 0.92]], c)
    const op = kf([[0, 0.8], [0.5, 1], [1, 0.8]], c)
    ctx.save()
    ctx.globalAlpha = op
    ctx.translate(cx, cy)
    ctx.scale(sc, sc)
    ctx.fillStyle = '#ffb445'
    ctx.font = `800 ${(S * 0.085).toFixed(1)}px system-ui, sans-serif`
    ctx.textAlign = 'center'
    ctx.textBaseline = 'middle'
    ctx.shadowColor = 'rgba(255,170,60,0.95)'
    ctx.shadowBlur = S * 0.018
    ctx.fillText('?', 0, S * 0.004)
    ctx.restore()
  }

  // -------------------------------------------------------------------------
  // Ground ring (base + pulse + evolution tiers).
  // -------------------------------------------------------------------------
  private drawGroundRing(now: number, S: number, evolved: boolean, tier: number) {
    const ctx = this.ctx
    const p = this.props
    const cx = 0.5 * S
    // Centre the ring cluster in the box (was 0.64) so the largest ring + its glow
    // stays fully inside the S×S canvas — otherwise the bottom arc is clipped by
    // the canvas edge in both the preview and the desktop-pet window.
    const cy = 0.5 * S

    // base
    ctx.save()
    const baseR = 0.34 * S
    const baseGlow = evolved ? this.rgb('rgb', 0.38) : 'rgba(96,178,255,0.3)'
    ctx.shadowColor = baseGlow
    ctx.shadowBlur = evolved ? S * 0.07 : S * 0.045
    const bg = ctx.createRadialGradient(cx, cy, 0, cx, cy, baseR)
    if (evolved) {
      bg.addColorStop(0, this.rgb('alt', 0.28))
      bg.addColorStop(0.38, this.rgb('rgb', 0.22))
      bg.addColorStop(0.58, this.rgb('rgb', 0.06))
      bg.addColorStop(0.74, 'rgba(0,0,0,0)')
    } else {
      bg.addColorStop(0, 'rgba(78,150,255,0.12)')
      bg.addColorStop(0.38, 'rgba(78,150,255,0.12)')
      bg.addColorStop(0.56, 'rgba(78,150,255,0.03)')
      bg.addColorStop(0.72, 'rgba(0,0,0,0)')
    }
    ctx.fillStyle = bg
    ctx.beginPath()
    ctx.ellipse(cx, cy, baseR, baseR, 0, 0, TAU)
    ctx.fill()
    ctx.restore()


    if (!evolved) return

    // Evolution tier rings (1..5). Each ring: dots on a circle, active tiers spin.
    const DOTS = p.lite ? 48 : 72
    const RING_DEF = [
      { size: 0.28, radius: 0.14, dot: 0.021, tail: 0.058, speed: 5600, delay: 0 },
      { size: 0.4, radius: 0.2, dot: 0.023, tail: 0.066, speed: 6600, delay: -120 },
      { size: 0.52, radius: 0.26, dot: 0.025, tail: 0.074, speed: 7600, delay: -240 },
      { size: 0.64, radius: 0.32, dot: 0.027, tail: 0.082, speed: 8600, delay: -360 },
      { size: 0.76, radius: 0.38, dot: 0.03, tail: 0.092, speed: 9600, delay: -480 },
    ]
    const power = this.ePower(tier)
    for (let ring = 1; ring <= 5; ring++) {
      const def = RING_DEF[ring - 1]
      const active = tier >= ring
      const radius = def.radius * S
      // Bolder level rings: thicken each dash radially and lengthen it a touch so
      // the rings read as solid bands instead of thin dotted lines.
      const dotH = def.dot * S * 1.9
      const tailW = def.tail * S * 1.2
      // Level rings only ROTATE now — no scale zoom, no opacity pulse.
      let rot = 0
      if (active && !p.reducedMotion) {
        const dir = ring === 2 || ring === 4 ? -1 : 1
        rot = dir * this.cycle(now, def.speed, def.delay) * TAU
      }
      const ringAlpha = active ? 0.8 + power * 0.18 : 0.55

      const realm = REALM[p.evolutionRealm as Exclude<FoxRealm, ''>]
      const [r, gg, b] = realm.rgb
      const [ar, ag, ab] = realm.alt

      ctx.save()
      ctx.translate(cx, cy)
      ctx.rotate(rot)
      ctx.globalAlpha = ringAlpha
      // Halo: the DOM dots carry a double box-shadow glow. Approximate it once per
      // ring (persists across every dot fill) so the rings glow like the DOM.
      if (active) {
        ctx.shadowColor = `rgba(${ar},${ag},${ab},${(0.5 + power * 0.32).toFixed(3)})`
        ctx.shadowBlur = S * 0.05
      } else {
        ctx.shadowColor = 'rgba(148,163,184,0.35)'
        ctx.shadowBlur = S * 0.02
      }
      for (let d = 0; d < DOTS; d++) {
        const ang = (d / DOTS) * TAU
        const dxp = Math.cos(ang) * radius
        const dyp = Math.sin(ang) * radius
        ctx.save()
        ctx.translate(dxp, dyp)
        ctx.rotate(ang + Math.PI / 2)
        // tapered bar: transparent → colour towards the outer end.
        const g = ctx.createLinearGradient(-tailW / 2, 0, tailW / 2, 0)
        if (active) {
          g.addColorStop(0, `rgba(${r},${gg},${b},0)`)
          g.addColorStop(0.34, `rgba(${r},${gg},${b},0.5)`)
          g.addColorStop(0.74, `rgba(${ar},${ag},${ab},0.95)`)
          g.addColorStop(1, 'rgba(255,255,255,0.98)')
        } else {
          g.addColorStop(0, 'rgba(148,163,184,0)')
          g.addColorStop(0.38, 'rgba(148,163,184,0.2)')
          g.addColorStop(1, 'rgba(148,163,184,0.55)')
        }
        ctx.fillStyle = g
        this.roundRect(-tailW / 2, -dotH / 2, tailW, dotH, dotH / 2)
        ctx.fill()
        ctx.restore()
      }
      ctx.restore()
    }
  }

  private rgb(which: 'rgb' | 'alt', alpha: number): string {
    const realm = REALM[this.props.evolutionRealm as Exclude<FoxRealm, ''>]
    if (!realm) return `rgba(96,178,255,${alpha})`
    const [r, g, b] = which === 'rgb' ? realm.rgb : realm.alt
    return `rgba(${r},${g},${b},${alpha})`
  }

}
