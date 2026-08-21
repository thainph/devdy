<script setup lang="ts">
/**
 * DY — Cyber Fox 2.5D mascot.
 * Layered, browser-native animation (transform/opacity only, no JS rAF loop).
 * Assets: fox-assets/generated/layers-2_5d (co-registered 1024² layers + manifest).
 * Public API unchanged: state / size / label / reducedMotion.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { mascotSpeaking } from '@/composables/useMascotSpeaking'

type CyberFoxState =
  | 'idle' | 'thinking' | 'loading'
  | 'success' | 'error' | 'permission' | 'syncing' | 'sleep'

const props = withDefaults(defineProps<{
  state?: CyberFoxState
  size?: 'sm' | 'md' | 'lg' | number
  label?: string
  reducedMotion?: boolean
  /** Number of orbiting light streams (one per running run) — only shown while orchestrating. */
  streams?: number
}>(), {
  state: 'idle',
  size: 'md',
  label: '',
  reducedMotion: false,
  streams: 1,
})

// Follow the shared speaking signal for THIS window. The sound player sets it in
// the main window; the desktop-pet window sets it from a forwarded event
// (MascotWindow → setSpeaking), so this component never needs a prop.
const isSpeaking = computed(() => mascotSpeaking.value)

// --- talking mouth flap -------------------------------------------------
// While speaking, only the head IMAGE is swapped for a co-registered sprite that
// cycles through mouth shapes; the rig's ears + eyes stay overlaid on top (so
// the eyes still follow the fox state via the normal eye system). Frames 0..3
// are the open-eye set whose baked eyes sit exactly under the rig eyes, closed →
// small → open → wide. The head stays fixed; only the mouth moves.
const TALK_SEQ = [0, 1, 2, 3, 2, 1]
const talkStep = ref(0)
// Only swap to the talking sprite when we can actually animate it.
const showTalk = computed(() => isSpeaking.value && !props.reducedMotion)
const talkFrame = computed(() => A_TALK[TALK_SEQ[talkStep.value % TALK_SEQ.length]] || A_TALK[0])
let talkTimer: ReturnType<typeof setInterval> | null = null
function stopTalk() {
  if (talkTimer) { clearInterval(talkTimer); talkTimer = null }
  talkStep.value = 0
}
watch(showTalk, (on) => {
  stopTalk()
  if (on) talkTimer = setInterval(() => { talkStep.value += 1 }, 110)
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
// Talking-head mouth-flap frames (full head sprite: ears + eyes + mouth baked).
const A_TALK = Array.from({ length: 12 }, (_, i) => A(`talk/${String(i).padStart(2, '0')}.webp`))

const originVars = {
  '--o-head': origin(manifest?.layers?.head?.pivot),
  '--o-earL': origin(manifest?.layers?.earLeft?.pivot),
  '--o-earR': origin(manifest?.layers?.earRight?.pivot),
  '--o-tailO': origin(manifest?.tails?.tailOrange?.pivot),
  '--o-tailB': origin(manifest?.tails?.tailBlue?.pivot),
  '--o-body': origin(manifest?.layers?.body?.pivot),
} as Record<string, string>

// ---- state → sprite mapping -------------------------------------------
const PROCESSING = new Set<CyberFoxState>(['loading', 'thinking', 'syncing'])
const showProcessingEyes = computed(() => PROCESSING.has(props.state) && !props.reducedMotion)

const eyeStatic = computed<string>(() => {
  switch (props.state) {
    case 'success': return 'happy'
    case 'error': return 'error'
    case 'sleep': return 'blink'
    case 'loading': case 'thinking': case 'syncing': return 'processing-02'
    default: return 'idle'
  }
})
// Only image-backed chest states reach the <img>; thinking/loading/success/
// error/permission are drawn with CSS (see .chest-fx variants).
const chestState = computed<string>(() => 'idle')
const showBlink = computed(() =>
  !showProcessingEyes.value && !['error', 'sleep', 'success'].includes(props.state))
const showOrchestration = computed(() =>
  ['loading', 'syncing'].includes(props.state))

// ---- size --------------------------------------------------------------
const sizePx = computed(() =>
  typeof props.size === 'number' ? `${Math.max(64, props.size)}px` : `${SIZES[props.size]}px`)
const sizeBucket = computed(() => {
  if (typeof props.size !== 'number') return props.size
  return props.size < 120 ? 'sm' : props.size < 176 ? 'md' : 'lg'
})

const rootStyle = computed(() => ({ '--dy-size': sizePx.value, ...originVars }))

const numericSize = computed(() =>
  typeof props.size === 'number' ? Math.max(64, props.size) : SIZES[props.size])

// ---- orbiting light streams -------------------------------------------
// One stream per running run (capped), each a trail of glow dots chasing its
// OWN tilted-ellipse orbit around the fox (head bright/large → tail small/dim).
const TRAIL = 9
const TRAIL_STEP = 0.028
const MAX_STREAMS = 6
// colour index cycles through the palette (.dot--c0..c5); first two match the tails
const streamCount = computed(() =>
  Math.max(1, Math.min(MAX_STREAMS, Math.round(props.streams || 1))))

// A tilted ellipse encircling the fox, sampled into an SVG path. Starts at the
// top (behind) so the depth toggle (dy-orbit-z) reads back → front → back.
function ellipsePath(tiltDeg: number, rxF: number, ryF: number): string {
  const S = numericSize.value
  const cx = 0.5 * S, cy = 0.52 * S, rx = rxF * S, ry = ryF * S
  const tilt = (tiltDeg * Math.PI) / 180, cos = Math.cos(tilt), sin = Math.sin(tilt)
  let d = ''
  for (let i = 0; i <= 48; i++) {
    const a = -Math.PI / 2 + (i / 48) * Math.PI * 2
    const x = rx * Math.cos(a), y = ry * Math.sin(a)
    const xr = (x * cos - y * sin + cx).toFixed(1)
    const yr = (x * sin + y * cos + cy).toFixed(1)
    d += `${i === 0 ? 'M' : 'L'} ${xr},${yr} `
  }
  return `path('${d}Z')`
}

const orbits = computed(() => {
  const n = streamCount.value
  return Array.from({ length: n }, (_, i) => {
    // spread tilts around the circle; jitter radii per stream so paths don't overlap
    const tilt = -34 + (i * 180) / n
    const rx = 0.4 + (i % 3) * 0.017
    const ry = 0.16 + ((i + 1) % 3) * 0.022
    return {
      id: i,
      path: ellipsePath(tilt, rx, ry),
      color: i % 6,
      phase: n > 1 ? i / n : 0, // stagger where each stream starts
      dur: 3000 + i * 280, // slightly different speeds → organic, non-synced
    }
  })
})

function dotStyle(orbit: { path: string; phase: number; dur: number }, i: number) {
  const p = orbit.phase - (i - 1) * TRAIL_STEP
  const frac = ((p % 1) + 1) % 1
  const k = (i - 1) / (TRAIL - 1) // 0 = head, 1 = tail
  const delay = `${(-frac * orbit.dur).toFixed(0)}ms`
  return {
    offsetPath: orbit.path,
    animationDuration: `${orbit.dur}ms, ${orbit.dur}ms`,
    animationDelay: `${delay}, ${delay}`,
    '--s': (1 - k * 0.58).toFixed(3),
    '--o': (1 - k * 0.72).toFixed(3),
  }
}

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
  stopTalk()
})
</script>

<template>
  <div
    ref="rootRef"
    class="cyber-fox"
    :class="{ 'cyber-fox--reduced': reducedMotion, 'cyber-fox--paused': paused, 'cyber-fox--speaking': isSpeaking }"
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
      <template v-for="orbit in orbits" :key="'orb' + orbit.id">
        <div
          v-for="i in TRAIL"
          :key="'orb' + orbit.id + '-' + i"
          class="l--fx orbit-dot"
          :class="'dot--c' + orbit.color"
          :style="dotStyle(orbit, i)"
        ></div>
      </template>
    </template>

    <!-- tails (base + delayed glow) -->
    <img class="l l--tailB tail-base" :src="A_TAILB" alt="" draggable="false" />
    <img class="l l--tailB tail-glow" :src="A_TAILB" alt="" draggable="false" />
    <img class="l l--tailO tail-base" :src="A_TAILO" alt="" draggable="false" />
    <img class="l l--tailO tail-glow" :src="A_TAILO" alt="" draggable="false" />

    <!-- body / head / ears -->
    <img class="l l--body breathe" :src="A_BODY" alt="" draggable="false" />
    <!-- head group: head + ears + eyes share the head motion. While a voice clip
         plays, ONLY the head image is swapped for a mouth-flap sprite; the rig's
         glowing cyber eyes and pointy ears stay overlaid on top, so the fox keeps
         its identity and the eyes still follow the fox state. -->
    <div class="l l--head-group head-move">
      <img v-if="!showTalk" class="l l--head" :src="A_HEAD" alt="" draggable="false" />
      <img v-else class="l l--head l--talk" :src="talkFrame" alt="" draggable="false" />
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
      <!-- thinking: a "typing"/pondering ellipsis instead of the generic processing icon -->
      <div v-if="state === 'thinking'" class="chest-fx chest-think" aria-hidden="true">
        <span></span><span></span><span></span>
      </div>
      <!-- loading (and running): an indeterminate progress bar sweeping across -->
      <div v-else-if="state === 'loading'" class="chest-fx chest-load" aria-hidden="true">
        <span class="chest-load-bar"></span>
      </div>
      <!-- success: green check pops in -->
      <div v-else-if="state === 'success'" class="chest-fx chest-ok" aria-hidden="true">
        <svg viewBox="0 0 24 24"><path d="M5 13l4 4L19 7" /></svg>
      </div>
      <!-- error: red cross shakes in -->
      <div v-else-if="state === 'error'" class="chest-fx chest-err" aria-hidden="true">
        <svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18" /></svg>
      </div>
      <!-- permission: amber question mark pulsing (waiting on the user) -->
      <div v-else-if="state === 'permission'" class="chest-fx chest-ask" aria-hidden="true">
        <span>?</span>
      </div>
      <img v-else class="chest-img" :src="A('chest/' + chestState + '.webp')" alt="" draggable="false" />
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

/* CSS chest indicators — share one mini terminal-panel base placed over the
   spot the baked chest icon occupies (~50%, 67%); each state fills it differently */
.chest-fx {
  position: absolute;
  left: 49.9%;
  top: 67.1%;
  transform: translate(-50%, -50%);
  width: 15%;
  aspect-ratio: 1.42 / 1;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 22%;
  background: radial-gradient(120% 120% at 50% 38%, #0b1836 0%, #050a1b 100%);
  box-shadow: 0 0 6px rgba(90,160,255,0.35), inset 0 0 4px rgba(90,160,255,0.28);
}

/* thinking: 3-dot "pondering" ellipsis */
.chest-think { gap: 8%; }
.chest-think span {
  width: 15%;
  aspect-ratio: 1;
  border-radius: 50%;
  background: radial-gradient(circle, #d3ecff 12%, #4aa3ff 70%);
  box-shadow: 0 0 4px rgba(120,190,255,0.9);
  animation: dy-think-dot 1.3s ease-in-out infinite;
}
.chest-think span:nth-child(2) { animation-delay: 0.16s; }
.chest-think span:nth-child(3) { animation-delay: 0.32s; }

/* loading: indeterminate bar sweeping across */
.chest-load { overflow: hidden; }
.chest-load-bar {
  position: absolute;
  top: 50%;
  left: 0;
  transform: translateY(-50%);
  width: 42%;
  height: 16%;
  border-radius: 999px;
  background: linear-gradient(90deg, rgba(120,190,255,0) 0%, #6bb6ff 50%, rgba(120,190,255,0) 100%);
  box-shadow: 0 0 5px rgba(120,190,255,0.85);
  animation: dy-load-bar 1.1s ease-in-out infinite;
}

/* success: green check pops in */
.chest-ok { box-shadow: 0 0 6px rgba(70,220,150,0.42), inset 0 0 4px rgba(70,220,150,0.3); }
.chest-ok svg {
  width: 48%; height: 48%;
  fill: none; stroke: #5be6a6; stroke-width: 3; stroke-linecap: round; stroke-linejoin: round;
  filter: drop-shadow(0 0 3px rgba(70,220,150,0.9));
  animation: dy-chest-pop 0.5s cubic-bezier(0.22,1,0.36,1) both;
}

/* error: red cross shakes in */
.chest-err { box-shadow: 0 0 6px rgba(255,90,90,0.42), inset 0 0 4px rgba(255,90,90,0.3); }
.chest-err svg {
  width: 46%; height: 46%;
  fill: none; stroke: #ff6b6b; stroke-width: 3; stroke-linecap: round; stroke-linejoin: round;
  filter: drop-shadow(0 0 3px rgba(255,90,90,0.9));
  animation: dy-chest-shake 0.5s ease-in-out both;
}

/* permission: amber question mark pulsing (waits on the user) */
.chest-ask {
  color: #ffb445;
  font-size: calc(var(--dy-size) * 0.085);
  font-weight: 800;
  line-height: 1;
  box-shadow: 0 0 6px rgba(255,170,60,0.42), inset 0 0 4px rgba(255,170,60,0.3);
}
.chest-ask span {
  text-shadow: 0 0 5px rgba(255,170,60,0.95);
  animation: dy-ask-pulse 1.5s ease-in-out infinite;
}

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
  /* Nudged left of dead-centre to compensate for the glow artwork being ~3%
     brighter on its right half, which otherwise reads as leaning right. */
  left: 47%;
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

/* loading: hard-switch the eyes through 01 → 02 → 03 continuously (no crossfade) */
.cyber-fox[data-state='loading'] .eyes-seq { animation: dy-eye-cycle var(--fox-eye) steps(1, end) infinite; }
.cyber-fox[data-state='loading'] .eyes-seq--2 { animation-delay: calc(var(--fox-eye) / -3); }
.cyber-fox[data-state='loading'] .eyes-seq--3 { animation-delay: calc(var(--fox-eye) / -1.5); }
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
/* per-stream colour palette (one hue per running run) */
.dot--c0 { background: radial-gradient(circle, rgba(255,226,184,0.98) 6%, rgba(255,168,80,0.6) 38%, rgba(255,150,60,0) 72%); }   /* amber */
.dot--c1 { background: radial-gradient(circle, rgba(206,238,255,0.98) 6%, rgba(112,190,255,0.6) 38%, rgba(96,178,255,0) 72%); }   /* blue */
.dot--c2 { background: radial-gradient(circle, rgba(198,255,236,0.98) 6%, rgba(80,230,180,0.6) 38%, rgba(60,220,160,0) 72%); }    /* teal */
.dot--c3 { background: radial-gradient(circle, rgba(233,210,255,0.98) 6%, rgba(180,130,255,0.6) 38%, rgba(160,110,255,0) 72%); }  /* violet */
.dot--c4 { background: radial-gradient(circle, rgba(255,214,236,0.98) 6%, rgba(255,120,190,0.6) 38%, rgba(255,100,175,0) 72%); }  /* pink */
.dot--c5 { background: radial-gradient(circle, rgba(224,255,206,0.98) 6%, rgba(160,235,90,0.6) 38%, rgba(150,225,70,0) 72%); }    /* lime */

/* --- state intensity tweaks --- */
/* thinking — curious: head tilt, ears perked, medium tail */
.cyber-fox[data-state='thinking'] { --tw: 1.15; }
.cyber-fox[data-state='thinking'] .head-move { animation: dy-head-ponder 2.8s ease-in-out infinite; }
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
/* pondering: head sways to BOTH sides with wider amplitude — "thinking it over" */
@keyframes dy-head-ponder {
  0%,100% { transform: rotate(-9deg) translate3d(-1.5%,0,0); }
  50%     { transform: rotate(9deg) translate3d(1.5%,-1%,0); }
}
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
/* each of the 3 layers is fully visible for exactly one third of the cycle */
@keyframes dy-eye-cycle { 0% { opacity: 1; } 33.33% { opacity: 0; } 66.66% { opacity: 0; } 100% { opacity: 0; } }
@keyframes dy-blink { 0%,95%,100% { opacity: 0; } 96.5%,98% { opacity: 0.9; } }
@keyframes dy-orbit-move { from { offset-distance: 0%; } to { offset-distance: 100%; } }
/* front (z90) only during the true front pass (point below body centre); back (z34,
   behind the body) the rest of the time — matches the tilted-ellipse geometry */
/* paths start at the top (behind); front pass is the middle half → wraps the fox */
@keyframes dy-orbit-z { 0%,24.9% { z-index: 34; } 25%,74.9% { z-index: 90; } 75%,100% { z-index: 34; } }
@keyframes dy-success { 0%,100% { transform: translate3d(0,0,0) scale(1); } 25% { transform: translate3d(0,-4%,0) scale(1.03); } 55% { transform: translate3d(0,0.5%,0) scale(0.995); } }
@keyframes dy-think-dot { 0%,60%,100% { opacity: 0.35; transform: translateY(8%) scale(0.82); } 30% { opacity: 1; transform: translateY(-26%) scale(1.12); } }
@keyframes dy-load-bar { 0% { left: -42%; } 100% { left: 100%; } }
@keyframes dy-chest-pop { 0% { opacity: 0; transform: scale(0.5); } 60% { transform: scale(1.15); } 100% { opacity: 1; transform: scale(1); } }
@keyframes dy-chest-shake { 0%,100% { transform: translateX(0) rotate(0deg); } 20% { transform: translateX(-12%) rotate(-4deg); } 40% { transform: translateX(10%) rotate(3deg); } 60% { transform: translateX(-7%) rotate(-2deg); } 80% { transform: translateX(4%) rotate(0deg); } }
@keyframes dy-ask-pulse { 0%,100% { transform: scale(0.92); opacity: 0.8; } 50% { transform: scale(1.08); opacity: 1; } }

/* --- size simplification --- */
.cyber-fox[data-size='sm'] .l--fx,
.cyber-fox[data-size='sm'] .l--blink { display: none; }
.cyber-fox[data-size='sm'] .l--earL,
.cyber-fox[data-size='sm'] .l--earR { animation: none; }

/* --- visibility pause --- */
.cyber-fox--paused * { animation-play-state: paused !important; }

/* --- reduced motion --- */
.cyber-fox--reduced .l, .cyber-fox--reduced .chest-img, .cyber-fox--reduced .l--eyes, .cyber-fox--reduced .l--ground, .cyber-fox--reduced .chest-think span, .cyber-fox--reduced .chest-load-bar, .cyber-fox--reduced .chest-ok svg, .cyber-fox--reduced .chest-err svg, .cyber-fox--reduced .chest-ask span { animation: none !important; }
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
