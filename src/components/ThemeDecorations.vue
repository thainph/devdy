<script setup lang="ts">
/**
 * Animated decorative overlay for "scenic" color themes.
 *
 * The app's panels are opaque, so a background-layer scene (z-index -1) would be
 * fully occluded. Instead this renders as a NON-INTERACTIVE overlay ABOVE the UI
 * (Teleport to <body>, high z-index, pointer-events: none). Interaction passes
 * straight through.
 *
 * Single scenic theme: Mid-Autumn Night (midautumn), which combines everything:
 *  - CSS layer (`.scene`, blends screen/dark, multiply/light): corner moon +
 *    twinkling stars, hanging swaying lanterns, rising sky-lanterns (đèn trời)
 *    with a flickering flame, and drifting embers.
 *  - Canvas layer: a fireworks particle system — rockets launch, burst into
 *    several shapes, and fall with gravity (additive blend on dark).
 *
 * All animation auto-pauses under `prefers-reduced-motion` and when the window
 * is hidden.
 */
import { ref, computed, watch, nextTick, onBeforeUnmount } from 'vue'

const props = defineProps<{ active: boolean; theme: string }>()

// The single scenic theme (midautumn) shows every decorative group at once.
const isScene = computed(() => props.active && props.theme === 'midautumn')
const showMoon = isScene
const showHanging = isScene
const showRising = isScene
const showEmbers = isScene
const cssSceneOn = isScene

// Deterministic star field (percent positions across the whole window).
const STARS = [
  { top: 10, left: 20, size: 2, delay: 0 },
  { top: 16, left: 46, size: 3, delay: 1.4 },
  { top: 8, left: 66, size: 2, delay: 0.6 },
  { top: 24, left: 12, size: 2, delay: 2.1 },
  { top: 30, left: 36, size: 2, delay: 3.2 },
  { top: 13, left: 82, size: 2, delay: 1.9 },
  { top: 42, left: 58, size: 3, delay: 0.9 },
  { top: 55, left: 26, size: 2, delay: 2.7 },
  { top: 62, left: 74, size: 2, delay: 3.6 },
  { top: 48, left: 8, size: 2, delay: 1.1 },
  { top: 70, left: 44, size: 2, delay: 4.0 },
  { top: 36, left: 90, size: 2, delay: 2.4 },
]

// Sky-lanterns drifting upward. Each: horizontal position, timing, size, tint.
const LANTERNS = [
  { left: 12, delay: 0, duration: 22, size: 30, hue: 26 },
  { left: 28, delay: 6, duration: 27, size: 24, hue: 32 },
  { left: 44, delay: 3, duration: 24, size: 34, hue: 22 },
  { left: 61, delay: 10, duration: 30, size: 26, hue: 30 },
  { left: 77, delay: 2, duration: 26, size: 25, hue: 20 },
  { left: 90, delay: 8, duration: 21, size: 22, hue: 34 },
]

// Lantern (Đèn lồng) scene — lanterns hanging from the top edge, gently swaying.
// cord = px the lantern hangs below the top; hue ~ red (2–8) or gold (36–42).
const HANG_LANTERNS = [
  { left: 8, cord: 40, scale: 1.0, hue: 4, dur: 5.5, delay: 0 },
  { left: 22, cord: 76, scale: 0.8, hue: 38, dur: 6.4, delay: 1.2 },
  { left: 37, cord: 30, scale: 1.15, hue: 2, dur: 5.0, delay: 0.6 },
  { left: 52, cord: 62, scale: 0.9, hue: 40, dur: 6.0, delay: 2.0 },
  { left: 68, cord: 44, scale: 1.06, hue: 6, dur: 5.7, delay: 0.9 },
  { left: 84, cord: 86, scale: 0.78, hue: 36, dur: 6.8, delay: 1.6 },
]

// Warm embers drifting upward.
const EMBERS = [
  { left: 12, dur: 9, delay: 0, size: 3 },
  { left: 26, dur: 11, delay: 2.5, size: 2 },
  { left: 40, dur: 8.5, delay: 1.2, size: 3 },
  { left: 55, dur: 12, delay: 3.4, size: 2 },
  { left: 67, dur: 9.5, delay: 0.7, size: 2 },
  { left: 79, dur: 10.5, delay: 2.0, size: 3 },
  { left: 91, dur: 8, delay: 4.0, size: 2 },
]

/* ------------------------------------------------------------------------- *
 * Fireworks (Pháo hoa) — canvas particle system.
 * ------------------------------------------------------------------------- */
const fwCanvas = ref<HTMLCanvasElement | null>(null)
let ctx: CanvasRenderingContext2D | null = null
let rafId = 0
let running = false
let lastTs = 0
let lastLaunch = 0

type Particle = {
  x: number; y: number; vx: number; vy: number
  life: number; max: number
  hue: number; light: number; size: number
  grav: number; flicker: boolean
  // Secondary break: when > 0 and life reaches it, spawn a mini burst (pháo tầng).
  sec: number; secHue: number
}
type FwType = 'peony' | 'chrysanthemum' | 'willow' | 'ring' | 'crackle' | 'palm' | 'multibreak'
type Rocket = { x: number; y: number; vy: number; targetY: number; hue: number; light: number; type: FwType }
let particles: Particle[] = []
let rockets: Rocket[] = []

// Festive hues: red, gold, amber, green, cyan, violet, magenta.
const FW_HUES = [0, 40, 52, 130, 190, 275, 320]
// Burst shapes (peony weighted higher by repeating it).
const FW_TYPES: FwType[] = ['peony', 'peony', 'chrysanthemum', 'willow', 'ring', 'crackle', 'palm', 'multibreak']

function addParticle(
  x: number, y: number, vx: number, vy: number,
  max: number, hue: number, light: number, size: number,
  grav: number, flicker: boolean, sec = 0, secHue = 0,
) {
  particles.push({ x, y, vx, vy, life: 0, max, hue, light, size, grav, flicker, sec, secHue })
}

// Small secondary burst used by the multibreak (pháo tầng) carriers.
function explodeMini(x: number, y: number, hue: number) {
  if (particles.length > 1800) return
  const dark = darkMode()
  const n = 14 + ((Math.random() * 8) | 0)
  const speed = 1 + Math.random() * 1.3
  for (let i = 0; i < n; i++) {
    const a = (Math.PI * 2 * i) / n + Math.random() * 0.3
    addParticle(x, y, Math.cos(a) * speed, Math.sin(a) * speed,
      28 + Math.random() * 20, hue + (Math.random() * 20 - 10), dark ? 70 : 46, 1.2 + Math.random() * 1.1, 0.03, false)
  }
}

const reducedMotion = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches
const darkMode = () => document.documentElement.classList.contains('dark')

function resizeCanvas() {
  const c = fwCanvas.value
  if (!c || !ctx) return
  const dpr = Math.min(window.devicePixelRatio || 1, 2)
  c.width = Math.floor(window.innerWidth * dpr)
  c.height = Math.floor(window.innerHeight * dpr)
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
}

function launchRocket() {
  const w = window.innerWidth
  const h = window.innerHeight
  const hue = FW_HUES[Math.floor(Math.random() * FW_HUES.length)]
  // Burst point in the upper part of the window, then pick an upward speed that
  // reaches it in ~1s (climb straight up, no gravity, so it never bursts early).
  const targetY = h * (0.14 + Math.random() * 0.34)
  const frames = 56 + Math.random() * 34
  rockets.push({
    x: w * (0.12 + Math.random() * 0.76),
    y: h,
    vy: -(h - targetY) / frames,
    targetY,
    hue,
    light: darkMode() ? 72 : 48,
    type: FW_TYPES[Math.floor(Math.random() * FW_TYPES.length)],
  })
}

function explode(x: number, y: number, hue: number, type: FwType) {
  if (particles.length > 1800) return
  const dark = darkMode()
  const light = dark ? 62 : 46

  if (type === 'ring') {
    // Clean expanding circle: even angles, uniform speed, low gravity.
    const n = 46
    const speed = 2.4 + Math.random() * 0.6
    for (let i = 0; i < n; i++) {
      const a = (Math.PI * 2 * i) / n
      addParticle(x, y, Math.cos(a) * speed, Math.sin(a) * speed,
        72 + Math.random() * 24, hue + (Math.random() * 10 - 5), light, 1.6 + Math.random() * 1.1, 0.018, false)
    }
  } else if (type === 'willow') {
    // Golden willow: slow, heavy gravity + long life → drooping trails.
    const n = 60 + ((Math.random() * 30) | 0)
    for (let i = 0; i < n; i++) {
      const a = Math.random() * Math.PI * 2
      const s = 1.1 + Math.random() * 1.3
      addParticle(x, y, Math.cos(a) * s, Math.sin(a) * s - 0.4,
        100 + Math.random() * 55, 45 + (Math.random() * 12 - 6), dark ? 66 : 48, 1.6 + Math.random() * 1.3, 0.055, false)
    }
  } else if (type === 'crackle') {
    // Glitter: many small, short-lived, flickering sparks.
    const n = 72 + ((Math.random() * 40) | 0)
    for (let i = 0; i < n; i++) {
      const a = Math.random() * Math.PI * 2
      const s = 0.8 + Math.random() * 2.4
      addParticle(x, y, Math.cos(a) * s, Math.sin(a) * s,
        30 + Math.random() * 38, hue + (Math.random() * 30 - 15), dark ? 72 : 46, 1 + Math.random() * 1.3, 0.03, true)
    }
  } else if (type === 'chrysanthemum') {
    // Dense two-tone bloom with trails.
    const n = 84 + ((Math.random() * 40) | 0)
    const hue2 = hue + 40
    for (let i = 0; i < n; i++) {
      const a = (Math.PI * 2 * i) / n + Math.random() * 0.12
      const s = 1.8 + Math.random() * 1.7
      addParticle(x, y, Math.cos(a) * s, Math.sin(a) * s,
        72 + Math.random() * 48, (i % 2 ? hue2 : hue) + (Math.random() * 16 - 8), light, 1.6 + Math.random() * 1.5, 0.028, false)
    }
  } else if (type === 'palm') {
    // Palm (pháo dừa): few thick fronds shooting up-and-out, then drooping.
    const n = 9 + ((Math.random() * 5) | 0)
    for (let i = 0; i < n; i++) {
      const a = (Math.PI * 2 * i) / n + Math.random() * 0.2
      const s = 2.6 + Math.random() * 1.7
      addParticle(x, y, Math.cos(a) * s, Math.sin(a) * s - 0.6,
        92 + Math.random() * 44, hue + (Math.random() * 10 - 5), dark ? 66 : 46, 2.4 + Math.random() * 1.7, 0.046, false)
    }
  } else if (type === 'multibreak') {
    // Multi-break (pháo tầng): a first bloom, plus carriers that burst again in
    // a contrasting colour after a short fuse.
    const n = 30 + ((Math.random() * 20) | 0)
    for (let i = 0; i < n; i++) {
      const a = (Math.PI * 2 * i) / n + Math.random() * 0.25
      const s = (1.5 + Math.random() * 1.4) * (0.5 + Math.random() * 0.7)
      addParticle(x, y, Math.cos(a) * s, Math.sin(a) * s,
        50 + Math.random() * 34, hue + (Math.random() * 22 - 11), light, 1.4 + Math.random() * 1.4, 0.03, false)
    }
    const carriers = 6 + ((Math.random() * 3) | 0)
    const secHue = hue + 150 // contrasting second colour
    for (let i = 0; i < carriers; i++) {
      const a = (Math.PI * 2 * i) / carriers + Math.random() * 0.15
      const s = 1.9 + Math.random() * 1.0
      addParticle(x, y, Math.cos(a) * s, Math.sin(a) * s,
        44, hue, dark ? 78 : 44, 2.1, 0.02, false, 24 + Math.random() * 12, secHue)
    }
  } else {
    // peony — classic round burst.
    const n = 52 + ((Math.random() * 34) | 0)
    const power = 1.6 + Math.random() * 1.6
    for (let i = 0; i < n; i++) {
      const a = (Math.PI * 2 * i) / n + Math.random() * 0.25
      const s = power * (0.4 + Math.random() * 0.8)
      addParticle(x, y, Math.cos(a) * s, Math.sin(a) * s,
        56 + Math.random() * 44, hue + (Math.random() * 26 - 13), light, 1.4 + Math.random() * 1.6, 0.03, false)
    }
  }
}

function frame(ts: number) {
  if (!running) return
  const cx = ctx
  if (!cx) { rafId = requestAnimationFrame(frame); return }
  const w = window.innerWidth
  const h = window.innerHeight
  const dt = lastTs ? Math.min((ts - lastTs) / 16.67, 2.5) : 1
  lastTs = ts

  // Trails: instead of clearing, fade the previous frame by lowering its alpha
  // (destination-out reduces the canvas's own alpha — no black is added to the
  // transparent overlay, so the UI stays clean). Lower alpha = longer trails.
  const dark = darkMode()
  cx.globalCompositeOperation = 'destination-out'
  cx.globalAlpha = 1
  cx.fillStyle = 'rgba(0,0,0,0.16)'
  cx.fillRect(0, 0, w, h)

  cx.globalCompositeOperation = dark ? 'lighter' : 'source-over'

  if (ts - lastLaunch > 780 + Math.random() * 720) {
    launchRocket()
    lastLaunch = ts
  }

  // Rockets climbing straight up toward their burst point.
  for (let i = rockets.length - 1; i >= 0; i--) {
    const r = rockets[i]
    r.y += r.vy * dt
    cx.globalAlpha = 0.9
    cx.fillStyle = `hsl(${r.hue} 90% ${r.light}%)`
    cx.beginPath()
    cx.arc(r.x, r.y, 1.8, 0, Math.PI * 2)
    cx.fill()
    if (r.y <= r.targetY) {
      explode(r.x, r.y, r.hue, r.type)
      rockets.splice(i, 1)
    }
  }

  // Burst particles with per-type gravity + drag + fade (+ optional flicker).
  for (let i = particles.length - 1; i >= 0; i--) {
    const p = particles[i]
    p.life += dt
    const t = p.life / p.max
    if (t >= 1) { particles.splice(i, 1); continue }
    p.x += p.vx * dt
    p.y += p.vy * dt
    p.vy += p.grav * dt
    p.vx *= 0.986
    p.vy *= 0.986
    // Secondary break (pháo tầng): carrier reaches its fuse → mini burst.
    if (p.sec && p.life >= p.sec) {
      explodeMini(p.x, p.y, p.secHue)
      p.sec = 0
    }
    let alpha = (1 - t) * (dark ? 1 : 0.8)
    if (p.flicker) alpha *= 0.35 + Math.random() * 0.65
    cx.globalAlpha = alpha
    cx.fillStyle = `hsl(${p.hue} 100% ${p.light}%)`
    cx.beginPath()
    cx.arc(p.x, p.y, p.size, 0, Math.PI * 2)
    cx.fill()
  }

  cx.globalAlpha = 1
  rafId = requestAnimationFrame(frame)
}

function startFireworks() {
  if (running || reducedMotion()) return
  const c = fwCanvas.value
  if (!c) return
  ctx = c.getContext('2d')
  if (!ctx) return
  resizeCanvas()
  running = true
  lastTs = 0
  lastLaunch = 0
  // Seed an immediate burst so there's instant feedback (a rocket takes ~1s to
  // climb before its first burst).
  explode(window.innerWidth * 0.5, window.innerHeight * 0.3, FW_HUES[1], 'chrysanthemum')
  window.addEventListener('resize', resizeCanvas)
  rafId = requestAnimationFrame(frame)
}

function stopFireworks() {
  running = false
  if (rafId) cancelAnimationFrame(rafId)
  rafId = 0
  particles = []
  rockets = []
  window.removeEventListener('resize', resizeCanvas)
  if (ctx) ctx.clearRect(0, 0, window.innerWidth, window.innerHeight)
}

const fireworksActive = () => props.active && props.theme === 'midautumn'

function onVisibility() {
  if (document.hidden) {
    if (running) {
      running = false
      if (rafId) cancelAnimationFrame(rafId)
      rafId = 0
    }
  } else if (fireworksActive() && !running && !reducedMotion()) {
    running = true
    lastTs = 0
    rafId = requestAnimationFrame(frame)
  }
}
document.addEventListener('visibilitychange', onVisibility)

watch(
  fireworksActive,
  (on) => {
    if (on) nextTick(startFireworks)
    else stopFireworks()
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  stopFireworks()
  document.removeEventListener('visibilitychange', onVisibility)
})
</script>

<template>
  <Teleport to="body">
    <!-- Unified CSS scene. Element groups toggle per theme; `midautumn` shows
         them all (plus the fireworks canvas below). -->
    <div v-if="cssSceneOn" class="scene" aria-hidden="true">
      <!-- Moon + stars (fullmoon / midautumn) -->
      <template v-if="showMoon">
        <div class="moon">
          <span class="crater crater-1" />
          <span class="crater crater-2" />
          <span class="crater crater-3" />
        </div>
        <span
          v-for="(s, i) in STARS"
          :key="'s' + i"
          class="star"
          :style="{
            top: s.top + '%',
            left: s.left + '%',
            width: s.size + 'px',
            height: s.size + 'px',
            animationDelay: s.delay + 's',
          }"
        />
      </template>

      <!-- Hanging swaying lanterns (lantern / midautumn) -->
      <div
        v-for="(l, i) in showHanging ? HANG_LANTERNS : []"
        :key="'h' + i"
        class="hang"
        :style="{
          left: l.left + '%',
          '--cord': l.cord + 'px',
          '--scale': l.scale,
          '--hue': l.hue,
          '--dur': l.dur + 's',
          animationDelay: '-' + l.delay + 's',
        }"
      >
        <span class="cord" />
        <span class="lbody">
          <span class="cap cap-top" />
          <span class="cap cap-bottom" />
          <span class="tassel" />
        </span>
      </div>

      <!-- Rising sky-lanterns / đèn trời (fullmoon / lantern / midautumn) -->
      <div
        v-for="(l, i) in showRising ? LANTERNS : []"
        :key="'l' + i"
        class="lantern"
        :style="{
          left: l.left + '%',
          width: l.size + 'px',
          height: l.size * 1.5 + 'px',
          '--hue': l.hue,
          '--rise-dur': l.duration + 's',
          '--rise-delay': l.delay + 's',
        }"
      >
        <span class="env" />
        <span class="lbase" />
        <span class="flame" />
      </div>

      <!-- Warm embers (lantern / midautumn) -->
      <span
        v-for="(e, i) in showEmbers ? EMBERS : []"
        :key="'e' + i"
        class="ember"
        :style="{
          left: e.left + '%',
          width: e.size + 'px',
          height: e.size + 'px',
          animationDuration: e.dur + 's',
          animationDelay: e.delay + 's',
        }"
      />
    </div>

    <!-- Fireworks canvas (fireworks / midautumn) -->
    <canvas
      v-if="active && theme === 'midautumn'"
      ref="fwCanvas"
      class="fw-canvas"
      aria-hidden="true"
    />
  </Teleport>
</template>

<style scoped>
.scene {
  position: fixed;
  inset: 0;
  z-index: 30;
  pointer-events: none;
  overflow: hidden;
}
/* Blend so the scene only tints, never washing out the UI or hurting text:
 * - Dark UI: `screen` → elements add light (glowing moon/lanterns).
 * - Light UI: `multiply` → elements only darken/tint (whites vanish, text stays
 *   dark and readable), so no more white haze. */
:global(html.dark) .scene {
  mix-blend-mode: screen;
}
:global(html:not(.dark)) .scene {
  mix-blend-mode: multiply;
}
/* Light-mode moon: saturated gold (no white core, which would vanish under
 * multiply) so it still reads as a warm corner glow. */
:global(html:not(.dark)) .moon {
  background: radial-gradient(
    circle at 42% 42%,
    hsl(45 95% 58% / 0.55),
    hsl(38 92% 50% / 0.38) 48%,
    hsl(38 90% 50% / 0) 72%
  );
}

/* --- Moon (top-right corner glow) --------------------------------------- */
.moon {
  position: absolute;
  top: -70px;
  right: -70px;
  width: 240px;
  height: 240px;
  border-radius: 50%;
  background: radial-gradient(
    circle at 42% 42%,
    hsl(48 100% 92% / 0.95),
    hsl(var(--primary) / 0.75) 45%,
    hsl(var(--primary) / 0) 72%
  );
  animation: moon-breath 8s ease-in-out infinite;
}
.crater {
  position: absolute;
  border-radius: 50%;
  background: hsl(var(--primary) / 0.35);
}
.crater-1 { width: 26px; height: 26px; top: 44%; left: 40%; }
.crater-2 { width: 16px; height: 16px; top: 62%; left: 56%; }
.crater-3 { width: 12px; height: 12px; top: 50%; left: 62%; }

/* --- Stars --------------------------------------------------------------- */
.star {
  position: absolute;
  border-radius: 50%;
  background: hsl(48 100% 92%);
  box-shadow: 0 0 6px 1px hsl(var(--primary) / 0.8);
  animation: twinkle 3.4s ease-in-out infinite;
}

/* --- Rising sky-lanterns (đèn trời Khổng Minh) --------------------------- *
 * A container that rises; children render the shape so the flame isn't clipped:
 *  .env   — paper envelope (clip-path balloon), glows + flickers.
 *  .lbase — small square fuel holder at the bottom opening.
 *  .flame — flickering candle flame below the opening. */
.lantern {
  position: absolute;
  bottom: -70px;
  animation: lantern-rise var(--rise-dur) ease-in var(--rise-delay) infinite;
}
.env {
  position: absolute;
  inset: 0;
  /* Rectangular (box) lantern: straight vertical sides the whole way down and a
   * flat bottom — no taper. Corners are lightly chamfered for a paper-box feel. */
  clip-path: polygon(
    14% 0%, 86% 0%,     /* flat top */
    100% 9%,            /* top-right chamfer */
    100% 91%,           /* straight vertical right side */
    86% 100%, 14% 100%, /* flat bottom (chamfered corners) */
    0% 91%,             /* straight vertical left side */
    0% 9%               /* top-left chamfer */
  );
  /* paper panel seams + candle glow rising from the bottom opening */
  background:
    linear-gradient(90deg, transparent 31%, hsl(calc(var(--hue) * 1deg) 55% 34% / 0.22) 33%, transparent 35%),
    linear-gradient(90deg, transparent 65%, hsl(calc(var(--hue) * 1deg) 55% 34% / 0.22) 67%, transparent 69%),
    radial-gradient(
      ellipse at 50% 92%,
      hsl(calc(var(--hue) * 1deg) 100% 90%),
      hsl(calc(var(--hue) * 1deg) 100% 68%) 38%,
      hsl(calc(var(--hue) * 1deg) 95% 56%) 74%,
      hsl(calc(var(--hue) * 1deg) 88% 48%) 100%
    );
  animation: env-flicker 0.5s ease-in-out infinite;
}
.lbase {
  position: absolute;
  left: 50%;
  bottom: -3%;
  transform: translateX(-50%);
  width: 15%;
  height: 9%;
  background: hsl(28 45% 26%);
  border: 1px solid hsl(40 70% 50%);
  border-radius: 1px;
}
.flame {
  position: absolute;
  left: 50%;
  bottom: 1%;
  transform: translateX(-50%);
  width: 13%;
  height: 18%;
  border-radius: 50% 50% 50% 50% / 34% 34% 66% 66%;
  background: radial-gradient(circle at 50% 72%, hsl(52 100% 94%), hsl(40 100% 66%) 58%, hsl(20 100% 56% / 0.7) 100%);
  box-shadow: 0 0 11px 4px hsl(40 100% 62% / 0.9);
  transform-origin: 50% 100%;
  animation: flame-flicker 0.42s ease-in-out infinite;
}

@keyframes moon-breath {
  0%, 100% { transform: scale(1); opacity: 0.9; }
  50% { transform: scale(1.05); opacity: 1; }
}
@keyframes twinkle {
  0%, 100% { opacity: 0.2; transform: scale(0.8); }
  50% { opacity: 0.95; transform: scale(1.2); }
}
@keyframes lantern-rise {
  0% { transform: translateY(0) translateX(0); opacity: 0; }
  12% { opacity: 0.85; }
  50% { transform: translateY(-52vh) translateX(14px); }
  85% { opacity: 0.85; }
  100% { transform: translateY(-108vh) translateX(-10px); opacity: 0; }
}
/* Candle light glowing through the paper envelope, flickering. */
@keyframes env-flicker {
  0%, 100% { filter: brightness(0.97) drop-shadow(0 0 9px hsl(35 95% 60% / 0.5)); }
  35% { filter: brightness(1.12) drop-shadow(0 0 13px hsl(38 100% 64% / 0.62)); }
  60% { filter: brightness(0.92) drop-shadow(0 0 8px hsl(32 95% 58% / 0.48)); }
  80% { filter: brightness(1.08) drop-shadow(0 0 12px hsl(40 100% 66% / 0.6)); }
}
/* Flame licking up and down irregularly (đốm lửa bập bùng). */
@keyframes flame-flicker {
  0%, 100% { transform: translateX(-50%) scale(1, 1); opacity: 0.9; }
  25% { transform: translateX(-53%) scale(0.88, 1.18); opacity: 1; }
  50% { transform: translateX(-49%) scale(1.06, 0.9); opacity: 0.82; }
  75% { transform: translateX(-47%) scale(0.92, 1.12); opacity: 1; }
}

/* --- Fireworks canvas ---------------------------------------------------- */
.fw-canvas {
  position: fixed;
  inset: 0;
  width: 100%;
  height: 100%;
  z-index: 30;
  pointer-events: none;
}
:global(html:not(.dark)) .fw-canvas {
  opacity: 0.75;
}

/* --- Lantern scene: hanging swaying lanterns + embers -------------------- */
.hang {
  position: absolute;
  top: 0;
  transform-origin: top center;
  animation: sway var(--dur) ease-in-out infinite;
}
.cord {
  display: block;
  width: 1.5px;
  height: var(--cord);
  margin: 0 auto;
  background: hsl(40 30% 62% / 0.5);
}
.lbody {
  position: relative;
  display: block;
  width: calc(30px * var(--scale));
  height: calc(40px * var(--scale));
  margin: 0 auto;
  border-radius: 50% / 46%;
  background:
    repeating-linear-gradient(
      90deg,
      transparent 0 4px,
      hsl(calc(var(--hue) * 1deg) 55% 32% / 0.25) 4px 5px
    ),
    radial-gradient(
      ellipse at 50% 40%,
      hsl(calc(var(--hue) * 1deg) 96% 70%),
      hsl(calc(var(--hue) * 1deg) 88% 52%) 62%,
      hsl(calc(var(--hue) * 1deg) 82% 42%) 100%
    );
  box-shadow:
    0 0 18px 4px hsl(calc(var(--hue) * 1deg) 92% 56% / 0.55),
    0 0 42px 10px hsl(calc(var(--hue) * 1deg) 90% 52% / 0.3);
  animation: lantern-glow calc(var(--dur) * 0.6) ease-in-out infinite;
}
.cap {
  position: absolute;
  left: 50%;
  transform: translateX(-50%);
  width: 42%;
  height: calc(4px * var(--scale));
  background: hsl(42 88% 56%);
  border-radius: 2px;
}
.cap-top { top: -3px; }
.cap-bottom { bottom: -3px; }
.tassel {
  position: absolute;
  top: 100%;
  left: 50%;
  transform: translateX(-50%);
  width: 2px;
  height: calc(11px * var(--scale));
  margin-top: 2px;
  background: linear-gradient(hsl(42 88% 56%), hsl(38 85% 48%));
  border-radius: 0 0 2px 2px;
}
.ember {
  position: absolute;
  bottom: -12px;
  border-radius: 50%;
  background: hsl(35 100% 66%);
  box-shadow: 0 0 8px 2px hsl(30 100% 55% / 0.8);
  animation: ember-rise linear infinite;
}

@keyframes sway {
  0%, 100% { transform: rotate(-4deg); }
  50% { transform: rotate(4deg); }
}
@keyframes lantern-glow {
  0%, 100% { filter: brightness(0.95); }
  50% { filter: brightness(1.18); }
}
@keyframes ember-rise {
  0% { transform: translateY(0) translateX(0) scale(1); opacity: 0; }
  12% { opacity: 0.9; }
  70% { opacity: 0.75; }
  100% { transform: translateY(-84vh) translateX(12px) scale(0.35); opacity: 0; }
}

@media (prefers-reduced-motion: reduce) {
  .moon,
  .star,
  .lantern,
  .hang,
  .lbody {
    animation: none !important;
  }
  .lantern,
  .ember {
    display: none;
  }
}
</style>
