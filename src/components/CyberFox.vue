<script setup lang="ts">
/**
 * DY — Cyber Fox 2.5D mascot.
 * Layered, browser-native animation (transform/opacity only, no JS rAF loop).
 * Assets: fox-assets/generated/layers-2_5d (co-registered 1024² layers + manifest).
 * Public API unchanged: state / size / label / reducedMotion.
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'

type CyberFoxState =
  | 'idle' | 'thinking' | 'loading' | 'running'
  | 'success' | 'error' | 'permission' | 'syncing' | 'sleep'

const props = withDefaults(defineProps<{
  state?: CyberFoxState
  size?: 'sm' | 'md' | 'lg' | number
  label?: string
  reducedMotion?: boolean
}>(), {
  state: 'idle',
  size: 'md',
  label: '',
  reducedMotion: false,
})

const SIZES = { sm: 96, md: 148, lg: 196 } as const

// ---- asset + manifest loading -----------------------------------------
const urlMap = import.meta.glob('../../fox-assets/generated/layers-2_5d/**/*.webp', {
  eager: true, query: '?url', import: 'default',
}) as Record<string, string>
const manifest = Object.values(import.meta.glob('../../fox-assets/generated/layers-2_5d/manifest.json', {
  eager: true, import: 'default',
}))[0] as any

function A(rel: string): string {
  const hit = Object.keys(urlMap).find((k) => k.endsWith('/' + rel))
  return hit ? urlMap[hit] : ''
}
function origin(p: { x: number; y: number } | undefined): string {
  return p ? `${(p.x * 100).toFixed(2)}% ${(p.y * 100).toFixed(2)}%` : '50% 50%'
}

const A_BODY = A('body.webp')
const A_HEAD = A('head.webp')
const A_EARL = A('ear-left.webp')
const A_EARR = A('ear-right.webp')
const A_TAILO = A('tail-orange.webp')
const A_TAILB = A('tail-blue.webp')
const A_GROUND = A('effects/ground-glow.webp')

const originVars = {
  '--o-head': origin(manifest?.layers?.head?.pivot),
  '--o-earL': origin(manifest?.layers?.earLeft?.pivot),
  '--o-earR': origin(manifest?.layers?.earRight?.pivot),
  '--o-tailO': origin(manifest?.tails?.tailOrange?.pivot),
  '--o-tailB': origin(manifest?.tails?.tailBlue?.pivot),
  '--o-body': origin(manifest?.layers?.body?.pivot),
} as Record<string, string>

// ---- state → sprite mapping -------------------------------------------
const PROCESSING = new Set<CyberFoxState>(['loading', 'thinking', 'running', 'syncing'])
const showProcessingEyes = computed(() => PROCESSING.has(props.state) && !props.reducedMotion)

const eyeStatic = computed<string>(() => {
  switch (props.state) {
    case 'success': return 'happy'
    case 'error': return 'error'
    case 'sleep': return 'blink'
    case 'loading': case 'thinking': case 'running': case 'syncing': return 'processing-02'
    default: return 'idle'
  }
})
const chestState = computed<string>(() => {
  switch (props.state) {
    case 'thinking': case 'loading': case 'running': return 'processing'
    case 'syncing': return 'syncing'
    case 'success': return 'success'
    case 'error': return 'error'
    case 'permission': return 'permission'
    default: return 'idle'
  }
})
const showBlink = computed(() =>
  !showProcessingEyes.value && !['error', 'sleep', 'success'].includes(props.state))
const showOrchestration = computed(() =>
  ['loading', 'running', 'syncing'].includes(props.state))

// orbiting trail: a string of glow dots chasing along the path (head bright/large → tail small/dim)
const TRAIL = 9
const TRAIL_STEP = 0.028
function dotStyle(i: number, phase: number) {
  const p = phase - (i - 1) * TRAIL_STEP
  const frac = ((p % 1) + 1) % 1
  const k = (i - 1) / (TRAIL - 1) // 0 = head, 1 = tail
  const delay = `calc(var(--fox-orbit) * ${(-frac).toFixed(4)})`
  return {
    offsetPath: orbitPath.value,
    animationDelay: `${delay}, ${delay}`,
    '--s': (1 - k * 0.58).toFixed(3),
    '--o': (1 - k * 0.72).toFixed(3),
  }
}

// ---- size --------------------------------------------------------------
const sizePx = computed(() =>
  typeof props.size === 'number' ? `${Math.max(64, props.size)}px` : `${SIZES[props.size]}px`)
const sizeBucket = computed(() => {
  if (typeof props.size !== 'number') return props.size
  return props.size < 120 ? 'sm' : props.size < 176 ? 'md' : 'lg'
})

const rootStyle = computed(() => ({ '--dy-size': sizePx.value, ...originVars }))

// diagonal tilted-ellipse orbit path (px in the root box, so it scales with size).
// back → left shoulder → across body → right foot → back behind.
const numericSize = computed(() =>
  typeof props.size === 'number' ? Math.max(64, props.size) : SIZES[props.size])
const orbitPath = computed(() => {
  const S = numericSize.value
  const cx = 0.5 * S, cy = 0.52 * S, rx = 0.42 * S, ry = 0.18 * S
  const tilt = -34 * Math.PI / 180, cos = Math.cos(tilt), sin = Math.sin(tilt)
  let d = ''
  for (let i = 0; i <= 48; i++) {
    const a = (i / 48) * Math.PI * 2
    const x = rx * Math.cos(a), y = ry * Math.sin(a)
    const xr = (x * cos - y * sin + cx).toFixed(1)
    const yr = (x * sin + y * cos + cy).toFixed(1)
    d += `${i === 0 ? 'M' : 'L'} ${xr},${yr} `
  }
  return `path('${d}Z')`
})

// ground ring sits at the feet, wide enough to embrace the fox
const groundStyle = computed(() => {
  const gW = manifest?.effects?.groundGlow?.width ?? 256
  return { width: `${(gW / 1024 * 100 * 1.5).toFixed(2)}%` }
})

// ---- visibility: pause when off-screen or window hidden ---------------
const rootRef = ref<HTMLElement | null>(null)
const paused = ref(false)
let io: IntersectionObserver | null = null
function onVisibility() { paused.value = document.hidden || paused.value }
onMounted(() => {
  if (typeof IntersectionObserver !== 'undefined' && rootRef.value) {
    io = new IntersectionObserver(([e]) => { paused.value = !e.isIntersecting || document.hidden }, { threshold: 0.01 })
    io.observe(rootRef.value)
  }
  document.addEventListener('visibilitychange', onVisibility)
})
onBeforeUnmount(() => {
  io?.disconnect()
  document.removeEventListener('visibilitychange', onVisibility)
})
</script>

<template>
  <div
    ref="rootRef"
    class="cyber-fox"
    :class="{ 'cyber-fox--reduced': reducedMotion, 'cyber-fox--paused': paused }"
    :data-state="state"
    :data-size="sizeBucket"
    :style="rootStyle"
    :aria-label="label || undefined"
    :aria-hidden="label ? undefined : 'true'"
    role="img"
  >
    <!-- ground (positioned at the feet, not full-canvas) -->
    <img class="l--ground" :src="A_GROUND" :style="groundStyle" alt="" draggable="false" />

    <!-- orbiting energy (pure CSS motion-path): two trails of glow dots chase diagonally,
         back → left shoulder → across body → right foot → back, crossing behind ↔ front -->
    <template v-if="showOrchestration">
      <div v-for="i in TRAIL" :key="'o' + i" class="l--fx orbit-dot dot--o" :style="dotStyle(i, 0)"></div>
      <div v-for="i in TRAIL" :key="'b' + i" class="l--fx orbit-dot dot--b" :style="dotStyle(i, 0.5)"></div>
    </template>

    <!-- tails (base + delayed glow) -->
    <img class="l l--tailB tail-base" :src="A_TAILB" alt="" draggable="false" />
    <img class="l l--tailB tail-glow" :src="A_TAILB" alt="" draggable="false" />
    <img class="l l--tailO tail-base" :src="A_TAILO" alt="" draggable="false" />
    <img class="l l--tailO tail-glow" :src="A_TAILO" alt="" draggable="false" />

    <!-- body / head / ears -->
    <img class="l l--body breathe" :src="A_BODY" alt="" draggable="false" />
    <!-- head group: head + ears + eyes share the head motion; ears/eyes add their own on top -->
    <div class="l l--head-group head-move">
      <img class="l l--head" :src="A_HEAD" alt="" draggable="false" />
      <img class="l l--earL" :src="A_EARL" alt="" draggable="false" />
      <img class="l l--earR" :src="A_EARR" alt="" draggable="false" />
      <div class="l l--eyes-wrap">
        <template v-if="showProcessingEyes">
          <img class="l--eyes eyes-seq eyes-seq--1" :src="A('eyes/processing-01.webp')" alt="" draggable="false" />
          <img class="l--eyes eyes-seq eyes-seq--2" :src="A('eyes/processing-02.webp')" alt="" draggable="false" />
          <img class="l--eyes eyes-seq eyes-seq--3" :src="A('eyes/processing-03.webp')" alt="" draggable="false" />
        </template>
        <template v-else>
          <img class="l--eyes" :src="A('eyes/' + eyeStatic + '.webp')" alt="" draggable="false" />
          <img v-if="showBlink" class="l--eyes l--blink" :src="A('eyes/blink.webp')" alt="" draggable="false" />
        </template>
      </div>
    </div>

    <!-- chest terminal — wrapper breathes with the body so it stays glued to the chest -->
    <div class="l l--chest-wrap breathe">
      <img class="chest-img" :src="A('chest/' + chestState + '.webp')" alt="" draggable="false" />
    </div>

    <span v-if="label" class="sr-only">{{ label }}</span>
  </div>
</template>

<style scoped>
.cyber-fox {
  --dy-size: 148px;
  --fox-loop: 3000ms;
  --fox-tail-o: 2600ms;
  --fox-tail-b: 2800ms;
  --tw: 1;
  --fox-chest: 1200ms;
  --fox-eye: 1200ms;
  --fox-orbit: 3200ms;
  position: relative;
  width: var(--dy-size);
  height: var(--dy-size);
  contain: layout paint;
  isolation: isolate;
  pointer-events: none;
  user-select: none;
}

.l {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: contain;
  backface-visibility: hidden;
  transform: translateZ(0);
  pointer-events: none;
  user-select: none;
}

/* z-order */
.l--tailB { z-index: 20; }
.l--tailO { z-index: 22; }
.l--body { z-index: 40; }
/* body + chest share one breathing driver so the chest never drifts from the body */
.breathe { transform-origin: var(--o-body); will-change: transform; animation: dy-breathe var(--fox-loop) ease-in-out infinite; }
.l--head-group { z-index: 50; }
.l--head { z-index: 50; }
.l--eyes-wrap { z-index: 70; }
/* head driver: head + ears + eyes move together around the neck pivot */
.head-move { transform-origin: var(--o-head); will-change: transform; animation: dy-head var(--fox-loop) ease-in-out infinite; }
.l--earL { z-index: 60; will-change: transform; animation: dy-ear-l var(--fox-loop) ease-in-out infinite; transform-origin: var(--o-earL); }
.l--earR { z-index: 61; will-change: transform; animation: dy-ear-r var(--fox-loop) ease-in-out infinite; transform-origin: var(--o-earR); }
.l--eyes { z-index: 70; }
.l--chest-wrap { z-index: 80; }
.chest-img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: contain; transform-origin: 50% 68%; animation: dy-chest var(--fox-chest) ease-in-out infinite; }

/* tails */
.l--tailO { transform-origin: var(--o-tailO); will-change: transform; }
.l--tailB { transform-origin: var(--o-tailB); will-change: transform; }
.l--tailO.tail-base { animation: dy-tail-o var(--fox-tail-o) ease-in-out infinite; }
.l--tailB.tail-base { animation: dy-tail-b var(--fox-tail-b) ease-in-out infinite; }
.tail-glow { mix-blend-mode: screen; opacity: 0; }
.l--tailO.tail-glow { animation: dy-tail-o var(--fox-tail-o) ease-in-out infinite, dy-glow var(--fox-tail-o) ease-in-out infinite; animation-delay: -140ms, -140ms; }
.l--tailB.tail-glow { animation: dy-tail-b var(--fox-tail-b) ease-in-out infinite, dy-glow var(--fox-tail-b) ease-in-out infinite; animation-delay: -160ms, -160ms; }

/* ground — anchored at the feet */
.l--ground {
  position: absolute;
  left: 50%;
  bottom: 3%;
  z-index: 0;
  height: auto;
  transform: translateX(-50%) scaleX(1);
  transform-origin: 50% 50%;
  mix-blend-mode: screen;
  object-fit: contain;
  animation: dy-ground var(--fox-loop) ease-in-out infinite;
}

/* eyes processing crossfade cycle */
.eyes-seq { animation: dy-eye-seq var(--fox-eye) ease-in-out infinite; }
.eyes-seq--2 { animation-delay: calc(var(--fox-eye) / -3); }
.eyes-seq--3 { animation-delay: calc(var(--fox-eye) / -1.5); }
.l--blink { animation: dy-blink 6s steps(1, end) infinite; opacity: 0; }

/* orbiting energy — pure CSS motion path. Each streak is a tapered light shard
   (pointed head, fat soft tail) that rides a diagonal ellipse and steps z-index
   behind ↔ in front of the body for a 3D pass. */
.orbit-dot {
  position: absolute;
  left: 0;
  top: 0;
  width: calc(11% * var(--s, 1));
  aspect-ratio: 1;
  offset-rotate: 0deg;
  offset-distance: 0%;
  opacity: var(--o, 1);
  border-radius: 50%;
  filter: blur(0.6px);
  mix-blend-mode: screen;
  pointer-events: none;
  will-change: offset-distance;
  animation: dy-orbit-move var(--fox-orbit) linear infinite, dy-orbit-z var(--fox-orbit) step-end infinite;
}
.dot--o { background: radial-gradient(circle, rgba(255,226,184,0.98) 6%, rgba(255,168,80,0.6) 38%, rgba(255,150,60,0) 72%); }
.dot--b { background: radial-gradient(circle, rgba(206,238,255,0.98) 6%, rgba(112,190,255,0.6) 38%, rgba(96,178,255,0) 72%); }

/* --- state intensity tweaks --- */
/* running — energetic: fast wide tail, perked ears, quick breathe */
.cyber-fox[data-state='running'] { --fox-tail-o: 1500ms; --fox-tail-b: 1650ms; --tw: 1.4; }
.cyber-fox[data-state='running'] .breathe { animation-duration: 1800ms; }
.cyber-fox[data-state='running'] .l--earL { animation: dy-ear-perk-l 0.72s ease-in-out infinite; }
.cyber-fox[data-state='running'] .l--earR { animation: dy-ear-perk-r 0.78s ease-in-out infinite; }

/* thinking — curious: head tilt, ears perked, medium tail */
.cyber-fox[data-state='thinking'] { --tw: 1.15; }
.cyber-fox[data-state='thinking'] .head-move { animation: dy-head-tilt 2.6s ease-in-out infinite; }
.cyber-fox[data-state='thinking'] .l--earL { animation: dy-ear-perk-l 1.5s ease-in-out infinite; }
.cyber-fox[data-state='thinking'] .l--earR { animation: dy-ear-perk-r 1.7s ease-in-out infinite; }

/* success — happy: nodding head, perked ears, fast wide tail wag */
.cyber-fox[data-state='success'] { --fox-tail-o: 900ms; --fox-tail-b: 960ms; --tw: 1.7; }
.cyber-fox[data-state='success'] .breathe { animation: dy-success 1.35s cubic-bezier(0.22,1,0.36,1) infinite; transform-origin: var(--o-body); }
.cyber-fox[data-state='success'] .head-move { animation: dy-head-nod 1s ease-in-out infinite; }
.cyber-fox[data-state='success'] .l--earL { animation: dy-ear-perk-l 0.85s ease-in-out infinite; }
.cyber-fox[data-state='success'] .l--earR { animation: dy-ear-perk-r 0.92s ease-in-out infinite; }

/* error — needs attention: head shake, drooped ears, limp tail */
.cyber-fox[data-state='error'] { --fox-tail-o: 3200ms; --fox-tail-b: 3400ms; --tw: 0.5; }
.cyber-fox[data-state='error'] .head-move { animation: dy-head-shake 0.8s ease-in-out infinite; }
.cyber-fox[data-state='error'] .l--earL { animation: dy-ear-droop-l 2s ease-in-out infinite; }
.cyber-fox[data-state='error'] .l--earR { animation: dy-ear-droop-r 2.15s ease-in-out infinite; }

/* permission — waiting for you: attentive tilt, perked ears */
.cyber-fox[data-state='permission'] .head-move { animation: dy-head-tilt 1.8s ease-in-out infinite; }
.cyber-fox[data-state='permission'] .l--earL { animation: dy-ear-perk-l 1.2s ease-in-out infinite; }
.cyber-fox[data-state='permission'] .l--earR { animation: dy-ear-perk-r 1.28s ease-in-out infinite; }

/* syncing — steady active tail */
.cyber-fox[data-state='syncing'] { --fox-tail-o: 1400ms; --fox-tail-b: 1520ms; --tw: 1.2; }

/* sleep — relaxed: drooped head + ears, slow small tail */
.cyber-fox[data-state='sleep'] { --fox-tail-o: 4200ms; --fox-tail-b: 4400ms; --tw: 0.6; }
.cyber-fox[data-state='sleep'] .breathe { animation-duration: 4600ms; }
.cyber-fox[data-state='sleep'] .head-move { animation: dy-head-droop 4.6s ease-in-out infinite; }
.cyber-fox[data-state='sleep'] .l--earL { animation: dy-ear-droop-l 4.4s ease-in-out infinite; }
.cyber-fox[data-state='sleep'] .l--earR { animation: dy-ear-droop-r 4.7s ease-in-out infinite; }

.cyber-fox[data-state='idle'] .chest-img,
.cyber-fox[data-state='sleep'] .chest-img { animation-duration: 2400ms; }

/* --- keyframes --- */
@keyframes dy-breathe { 0%,100% { transform: translate3d(0,0,0) scaleY(1); } 50% { transform: translate3d(0,-1.5%,0) scaleY(1.006); } }
@keyframes dy-head { 0%,100% { transform: translate3d(0,0,0) rotate(0deg); } 30% { transform: translate3d(0,-1%,0) rotate(-1deg); } 65% { transform: translate3d(0,-0.4%,0) rotate(1deg); } }
@keyframes dy-ear-l { 0%,20%,58%,100% { transform: rotate(0deg); } 33% { transform: rotate(-4deg); } 42% { transform: rotate(-1.2deg); } 50% { transform: rotate(-2.6deg); } }
@keyframes dy-ear-r { 0%,24%,62%,100% { transform: rotate(0deg); } 38% { transform: rotate(3.6deg); } 47% { transform: rotate(1deg); } 55% { transform: rotate(2.2deg); } }
@keyframes dy-tail-o { 0%,100% { transform: rotate(calc(-3deg * var(--tw))) scale(1); } 50% { transform: rotate(calc(6deg * var(--tw))) scale(1.03); } }
@keyframes dy-tail-b { 0%,100% { transform: rotate(calc(3deg * var(--tw))) scale(1); } 50% { transform: rotate(calc(-5.5deg * var(--tw))) scale(1.03); } }
/* expressive secondary motion */
@keyframes dy-ear-perk-l { 0%,100% { transform: rotate(-3deg); } 50% { transform: rotate(-7.5deg); } }
@keyframes dy-ear-perk-r { 0%,100% { transform: rotate(3deg); } 50% { transform: rotate(7.5deg); } }
@keyframes dy-ear-droop-l { 0%,100% { transform: rotate(6deg); } 50% { transform: rotate(9deg); } }
@keyframes dy-ear-droop-r { 0%,100% { transform: rotate(-6deg); } 50% { transform: rotate(-9deg); } }
@keyframes dy-head-tilt { 0%,100% { transform: rotate(-5deg) translate3d(0,0,0); } 50% { transform: rotate(-3deg) translate3d(0,-1%,0); } }
@keyframes dy-head-shake { 0%,100% { transform: rotate(0deg); } 18% { transform: rotate(-5deg); } 38% { transform: rotate(4.5deg); } 58% { transform: rotate(-3.5deg); } 78% { transform: rotate(2.5deg); } }
@keyframes dy-head-nod { 0%,100% { transform: translate3d(0,0,0) rotate(0deg); } 30% { transform: translate3d(0,-4.5%,0) rotate(0.5deg); } 60% { transform: translate3d(0,1%,0) rotate(-0.5deg); } }
@keyframes dy-head-droop { 0%,100% { transform: translate3d(0,1.5%,0) rotate(-2deg); } 50% { transform: translate3d(0,3%,0) rotate(-3deg); } }
@keyframes dy-glow { 0%,100% { opacity: 0.28; } 45% { opacity: 0.85; } 70% { opacity: 0.5; } }
@keyframes dy-chest { 0%,100% { transform: scale(1); } 50% { transform: scale(1.035); } }
/* ground ring: gently stretches outward to embrace the fox's feet */
@keyframes dy-ground {
  0%, 100% { opacity: 0.5; transform: translateX(-50%) scale(0.9); }
  50% { opacity: 0.82; transform: translateX(-50%) scale(1.14); }
}
@keyframes dy-eye-seq { 0%,22% { opacity: 1; } 40%,88% { opacity: 0; } 100% { opacity: 1; } }
@keyframes dy-blink { 0%,95%,100% { opacity: 0; } 96.5%,98% { opacity: 0.9; } }
@keyframes dy-orbit-move { from { offset-distance: 0%; } to { offset-distance: 100%; } }
/* front (z90) only during the true front pass (point below body centre); back (z34,
   behind the body) the rest of the time — matches the tilted-ellipse geometry */
@keyframes dy-orbit-z { 0% { z-index: 34; } 13% { z-index: 90; } 63% { z-index: 34; } }
@keyframes dy-success { 0%,100% { transform: translate3d(0,0,0) scale(1); } 25% { transform: translate3d(0,-4%,0) scale(1.03); } 55% { transform: translate3d(0,0.5%,0) scale(0.995); } }

/* --- size simplification --- */
.cyber-fox[data-size='sm'] .l--fx,
.cyber-fox[data-size='sm'] .l--blink { display: none; }
.cyber-fox[data-size='sm'] .l--earL,
.cyber-fox[data-size='sm'] .l--earR { animation: none; }

/* --- visibility pause --- */
.cyber-fox--paused * { animation-play-state: paused !important; }

/* --- reduced motion --- */
.cyber-fox--reduced .l, .cyber-fox--reduced .chest-img, .cyber-fox--reduced .l--eyes, .cyber-fox--reduced .l--ground { animation: none !important; }
.cyber-fox--reduced .l--fx, .cyber-fox--reduced .tail-glow, .cyber-fox--reduced .l--blink { display: none; }
@media (prefers-reduced-motion: reduce) {
  .cyber-fox .l, .cyber-fox .chest-img, .cyber-fox .l--eyes, .cyber-fox .l--ground { animation: none !important; }
  .cyber-fox .l--fx, .cyber-fox .tail-glow, .cyber-fox .l--blink { display: none; }
}

.sr-only {
  position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
  overflow: hidden; clip: rect(0,0,0,0); white-space: nowrap; border: 0;
}
</style>
