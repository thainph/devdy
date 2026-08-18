<script setup lang="ts">
/**
 * DEV-ONLY Cyber Fox 2.5D reconstruction / registration debug tool.
 * Modes A–E + Motion test + drag/rotate reposition + Save layout.
 * Not shipped to production (route guarded by import.meta.env.DEV).
 */
import { computed, reactive, ref } from 'vue'

const urlMap = import.meta.glob('../../../fox-assets/generated/layers-2_5d/**/*.webp', {
  eager: true, query: '?url', import: 'default',
}) as Record<string, string>
const manifestMap = import.meta.glob('../../../fox-assets/generated/layers-2_5d/manifest.json', {
  eager: true, import: 'default',
}) as Record<string, any>
const manifest = Object.values(manifestMap)[0] as any
const placement = manifest.placement ?? {}

function assetUrl(rel: string): string {
  const hit = Object.keys(urlMap).find((k) => k.endsWith('/' + rel))
  return hit ? urlMap[hit] : ''
}

type Mode = 'A' | 'B' | 'C' | 'D' | 'E'
const mode = ref<Mode>('B')
const MODES: { id: Mode; label: string }[] = [
  { id: 'A', label: 'A · Canonical' },
  { id: 'B', label: 'B · Reconstruction' },
  { id: 'C', label: 'C · 50/50' },
  { id: 'D', label: 'D · Boundaries' },
  { id: 'E', label: 'E · Pivots' },
]

const eyeState = ref('idle')
const chestState = ref('idle')
const eyeStates = computed(() => Object.keys(manifest.eyes ?? {}))
const chestStates = computed(() => Object.keys(manifest.chest ?? {}))

const STACK: { key: string; file: string; z: number; kind: string; label: string }[] = [
  { key: 'tailOrange', file: 'tail-orange.webp', z: 30, kind: 'tail', label: 'tail-orange' },
  { key: 'tailBlue', file: 'tail-blue.webp', z: 31, kind: 'tail', label: 'tail-blue' },
  { key: 'body', file: 'body.webp', z: 40, kind: 'body', label: 'body' },
  { key: 'head', file: 'head.webp', z: 50, kind: 'head', label: 'head' },
  { key: 'earLeft', file: 'ear-left.webp', z: 60, kind: 'ear', label: 'ear-left' },
  { key: 'earRight', file: 'ear-right.webp', z: 61, kind: 'ear', label: 'ear-right' },
  { key: 'eyes', file: '', z: 70, kind: 'eyes', label: 'eyes (pair)' },
  { key: 'chest', file: '', z: 80, kind: 'chest', label: 'chest' },
]

const visible = reactive<Record<string, boolean>>({
  tailOrange: true, tailBlue: true, body: true, head: true,
  earLeft: true, earRight: true, eyes: true, chest: true, ground: true, effects: false,
})

function pivotOf(key: string): { x: number; y: number } | null {
  const layers = manifest.layers ?? {}
  if (layers[key]?.pivot) return layers[key].pivot
  if (key === 'tailOrange') return manifest.tails?.tailOrange?.pivot ?? null
  if (key === 'tailBlue') return manifest.tails?.tailBlue?.pivot ?? null
  return null
}
// placement centre (used as rotate origin while editing)
function centerOf(key: string): { x: number; y: number } {
  if (key === 'eyes') return { x: 0.5, y: placement.eyeLeft?.cy ?? 0.427 }
  const p = placement[key]
  return { x: p?.cx ?? 0.5, y: p?.cy ?? 0.5 }
}
function originFor(key: string): string {
  const useCenter = edit.value || (!motion.value && hasOffset(key))
  const o = useCenter ? centerOf(key) : (pivotOf(key) ?? centerOf(key))
  return `${(o.x * 100).toFixed(2)}% ${(o.y * 100).toFixed(2)}%`
}

// ---- Edit / drag + rotate ---------------------------------------------
const STAGE = 560
const edit = ref(false)
const activePart = ref('head')
const offset = reactive<Record<string, { dx: number; dy: number; rot: number }>>({})
STACK.forEach((s) => { offset[s.key] = { dx: 0, dy: 0, rot: 0 } })
function hasOffset(key: string) { const o = offset[key]; return !!o && (o.dx || o.dy || o.rot) }

const stageRef = ref<HTMLElement | null>(null)
let drag: { startX: number; startY: number; base: { dx: number; dy: number }; size: number } | null = null

function onStageDown(e: PointerEvent) {
  if (!edit.value) return
  const size = stageRef.value?.clientWidth ?? STAGE
  const o = offset[activePart.value]
  drag = { startX: e.clientX, startY: e.clientY, base: { dx: o.dx, dy: o.dy }, size }
  ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
}
function onStageMove(e: PointerEvent) {
  if (!edit.value || !drag) return
  const o = offset[activePart.value]
  o.dx = drag.base.dx + (e.clientX - drag.startX) / drag.size
  o.dy = drag.base.dy + (e.clientY - drag.startY) / drag.size
}
function onStageUp(e: PointerEvent) { drag = null; (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId) }
function nudge(axis: 'dx' | 'dy', px: number) { offset[activePart.value][axis] += px / STAGE }
function rotateBy(deg: number) { offset[activePart.value].rot += deg }
function resetOffsets() { STACK.forEach((s) => { offset[s.key] = { dx: 0, dy: 0, rot: 0 } }) }

// ---- Save layout -------------------------------------------------------
const savedText = ref('')
function num(n: number) { return Number(n).toFixed(4).replace(/0+$/, '').replace(/\.$/, '.0') }
function computeLayout() {
  const p = placement, o = offset
  const wf = (k: string, def: number) => (p[k]?.wf ?? def)
  const cx = (k: string) => (p[k]?.cx ?? 0.5) + (o[k]?.dx ?? 0)
  const cy = (k: string) => (p[k]?.cy ?? 0.5) + (o[k]?.dy ?? 0)
  const rt = (k: string) => (p[k]?.rot ?? 0) + (o[k]?.rot ?? 0)
  const L: string[] = ['// LAYOUT (paste into scripts/build-cyber-fox-layers.mjs)']
  L.push(`  body:        { wf: ${num(wf('body', 0.42))}, cx: ${num(cx('body'))}, cy: ${num(cy('body'))}, rot: ${num(rt('body'))} },`)
  L.push(`  head:        { wf: ${num(wf('head', 0.44))}, cx: ${num(cx('head'))}, cy: ${num(cy('head'))}, rot: ${num(rt('head'))} },`)
  L.push(`  'ear-left':  { wf: ${num(wf('earLeft', 0.15))}, cx: ${num(cx('earLeft'))}, cy: ${num(cy('earLeft'))}, rot: ${num(rt('earLeft'))} },`)
  L.push(`  'ear-right': { wf: ${num(wf('earRight', 0.15))}, cx: ${num(cx('earRight'))}, cy: ${num(cy('earRight'))}, rot: ${num(rt('earRight'))} },`)
  L.push(`  'tail-orange': { wf: ${num(wf('tailOrange', 0.215))}, cx: ${num(cx('tailOrange'))}, cy: ${num(cy('tailOrange'))}, rot: ${num(rt('tailOrange'))} },`)
  L.push(`  'tail-blue':   { wf: ${num(wf('tailBlue', 0.235))}, cx: ${num(cx('tailBlue'))}, cy: ${num(cy('tailBlue'))}, rot: ${num(rt('tailBlue'))} },`)
  const eo = o.eyes || { dx: 0, dy: 0, rot: 0 }
  const elx = (p.eyeLeft?.cx ?? 0.415) + eo.dx, ely = (p.eyeLeft?.cy ?? 0.427) + eo.dy
  const erx = (p.eyeRight?.cx ?? 0.585) + eo.dx, ery = (p.eyeRight?.cy ?? 0.427) + eo.dy
  L.push(`// EYE = { left: { cx: ${num(elx)}, cy: ${num(ely)} }, right: { cx: ${num(erx)}, cy: ${num(ery)} } ; EYE_ROT = ${num((p.eyeLeft?.rot ?? 0) + eo.rot)}`)
  L.push(`// CHEST_PLACE = { wf: ${num(wf('chest', 0.15))}, cx: ${num(cx('chest'))}, cy: ${num(cy('chest'))}, rot: ${num(rt('chest'))} }`)
  return L.join('\n')
}
function saveLayout() {
  savedText.value = computeLayout()
  try { navigator.clipboard?.writeText(savedText.value) } catch { /* ignore */ }
  const blob = new Blob([savedText.value], { type: 'text/plain' })
  const a = document.createElement('a')
  a.href = URL.createObjectURL(blob); a.download = 'cyber-fox-layout.txt'; a.click()
  URL.revokeObjectURL(a.href)
}

// ---- motion test -------------------------------------------------------
const motion = ref(false)
const amp = reactive({ head: 3, ear: 2, tail: 5 })
const t = ref(0)
let raf = 0
function loop() { t.value = (t.value + 0.016) % 1000; if (motion.value) raf = requestAnimationFrame(loop) }
function toggleMotion() {
  motion.value = !motion.value
  if (motion.value) { edit.value = false; raf = requestAnimationFrame(loop) } else cancelAnimationFrame(raf)
}
function toggleEdit() { edit.value = !edit.value; if (edit.value && motion.value) { motion.value = false; cancelAnimationFrame(raf) } }
function wave(speed = 1, phase = 0) { return Math.sin((t.value * speed + phase) * Math.PI * 2) }
const motionRot = computed(() => {
  const m: Record<string, number> = {}
  if (!motion.value) return m
  m.head = wave(0.5) * amp.head
  m.earLeft = (wave(0.7) - 1) / 2 * amp.ear
  m.earRight = (wave(0.7, 0.2) + 1) / 2 * amp.ear
  m.tailOrange = wave(0.4) * amp.tail
  m.tailBlue = -wave(0.4, 0.1) * amp.tail
  return m
})
function transformOf(key: string): string {
  const o = offset[key] || { dx: 0, dy: 0, rot: 0 }
  const move = (o.dx || o.dy) ? `translate(${(o.dx * 100).toFixed(3)}%, ${(o.dy * 100).toFixed(3)}%)` : ''
  if (motion.value && !edit.value) {
    const r = motionRot.value[key] ?? 0
    return `${move}${r ? ` rotate(${r.toFixed(2)}deg)` : ''}` || 'none'
  }
  const r = o.rot ? ` rotate(${o.rot.toFixed(2)}deg)` : ''
  return `${move}${r}` || 'none'
}

function fileFor(item: { key: string; file: string; kind: string }): string {
  if (item.kind === 'eyes') return assetUrl(`eyes/${eyeState.value}.webp`)
  if (item.kind === 'chest') return assetUrl(`chest/${chestState.value}.webp`)
  return assetUrl(item.file)
}

const showReconstruction = computed(() => mode.value !== 'A')
const showCanonical = computed(() => mode.value === 'A' || mode.value === 'C')
const effects = computed(() => Object.entries(manifest.effects ?? {}).map(([k, v]: any) => ({ key: k, ...v })))
</script>

<template>
  <div class="cfd">
    <header class="cfd__bar">
      <strong>Cyber Fox 2.5D · Debug</strong>
      <div class="cfd__modes">
        <button v-for="m in MODES" :key="m.id" :class="{ on: mode === m.id }" @click="mode = m.id">{{ m.label }}</button>
      </div>
      <label class="cfd__mt"><input type="checkbox" :checked="edit" @change="toggleEdit" /> Edit (drag/rotate)</label>
      <label class="cfd__mt"><input type="checkbox" :checked="motion" @change="toggleMotion" /> Motion</label>
    </header>

    <div class="cfd__body">
      <aside class="cfd__panel">
        <template v-if="edit">
          <h4>Active part</h4>
          <select v-model="activePart">
            <option v-for="s in STACK" :key="s.key" :value="s.key">{{ s.label }}</option>
          </select>
          <h4>Move (1px)</h4>
          <div class="cfd__pad">
            <button @click="nudge('dy', -1)">↑</button>
            <button @click="nudge('dx', -1)">←</button>
            <button @click="nudge('dx', 1)">→</button>
            <button @click="nudge('dy', 1)">↓</button>
          </div>
          <h4>Rotate</h4>
          <div class="cfd__rot">
            <button @click="rotateBy(-1)">↺ -1°</button>
            <button @click="rotateBy(1)">↻ +1°</button>
            <button @click="rotateBy(-0.2)">-0.2°</button>
            <button @click="rotateBy(0.2)">+0.2°</button>
            <span>rot: {{ (offset[activePart].rot).toFixed(2) }}°</span>
          </div>
          <div class="cfd__save">
            <button class="primary" @click="saveLayout">💾 Save layout</button>
            <button @click="resetOffsets">Reset</button>
          </div>
          <textarea v-if="savedText" class="cfd__out" readonly :value="savedText"></textarea>
          <p class="cfd__hint">Kéo trên stage để di chuyển, dùng nút để xoay “{{ activePart }}”. Save → copy clipboard + tải .txt, gửi tôi.</p>
        </template>

        <template v-else>
          <h4>Layers</h4>
          <label v-for="s in STACK" :key="s.key"><input type="checkbox" v-model="visible[s.key]" /> {{ s.label }}</label>
          <label><input type="checkbox" v-model="visible.ground" /> ground</label>
          <label><input type="checkbox" v-model="visible.effects" /> effects</label>
          <h4>Eyes</h4>
          <select v-model="eyeState"><option v-for="e in eyeStates" :key="e" :value="e">{{ e }}</option></select>
          <h4>Chest</h4>
          <select v-model="chestState"><option v-for="c in chestStates" :key="c" :value="c">{{ c }}</option></select>
          <template v-if="motion">
            <h4>Amplitude</h4>
            <label>head ±{{ amp.head }}°<input type="range" min="0" max="8" step="0.5" v-model.number="amp.head" /></label>
            <label>ear ±{{ amp.ear }}°<input type="range" min="0" max="6" step="0.5" v-model.number="amp.ear" /></label>
            <label>tail ±{{ amp.tail }}°<input type="range" min="0" max="10" step="0.5" v-model.number="amp.tail" /></label>
          </template>
        </template>
      </aside>

      <main class="cfd__stagewrap">
        <div ref="stageRef" class="cfd__stage" :class="{ editing: edit }" :data-mode="mode"
             @pointerdown="onStageDown" @pointermove="onStageMove" @pointerup="onStageUp" @pointercancel="onStageUp">
          <img v-if="visible.ground" class="cfd__fx" :style="{ left: '50%', top: '90%', width: (manifest.effects?.groundGlow?.width/1024*100)+'%' }"
               :src="assetUrl('effects/ground-glow.webp')" alt="" />
          <img v-if="showCanonical" class="cfd__full" :class="{ half: mode === 'C' }" :src="assetUrl('canonical-composite.webp')" alt="canonical" />

          <template v-if="showReconstruction">
            <div v-for="s in STACK" :key="s.key" v-show="visible[s.key]" class="cfd__layer"
                 :class="{ half: mode === 'C', bound: mode === 'D', active: edit && activePart === s.key }"
                 :style="{ zIndex: s.z, transformOrigin: originFor(s.key), transform: transformOf(s.key) }">
              <img class="cfd__full" :src="fileFor(s)" alt="" />
              <span v-if="mode === 'E' && pivotOf(s.key)" class="cfd__pivot"
                    :style="{ left: (pivotOf(s.key)!.x*100)+'%', top: (pivotOf(s.key)!.y*100)+'%' }" />
              <span v-if="mode === 'D' || (edit && activePart === s.key)" class="cfd__tag">{{ s.label }}</span>
            </div>
          </template>

          <template v-if="visible.effects">
            <img v-for="fx in effects" :key="fx.key" class="cfd__fx"
                 :style="{ left: (fx.recommendedPlacement.x*100)+'%', top: (fx.recommendedPlacement.y*100)+'%', width: (fx.width/1024*100)+'%', zIndex: 90 }"
                 :src="assetUrl(fx.file)" alt="" />
          </template>
        </div>
      </main>
    </div>
  </div>
</template>

<style scoped>
.cfd { height: 100%; display: flex; flex-direction: column; background: #0b0d17; color: #e7e9f3; }
.cfd__bar { display: flex; align-items: center; gap: 16px; padding: 10px 16px; border-bottom: 1px solid #232842; }
.cfd__modes { display: flex; gap: 6px; }
.cfd__modes button { padding: 4px 10px; border-radius: 6px; border: 1px solid #2b3157; background: #151a2e; color: #b7bde0; cursor: pointer; }
.cfd__modes button.on { background: #4f46e5; color: #fff; border-color: #4f46e5; }
.cfd__mt { font-size: 13px; }
.cfd__bar .cfd__mt:first-of-type { margin-left: auto; }
.cfd__body { flex: 1; display: flex; min-height: 0; }
.cfd__panel { width: 250px; padding: 12px 16px; border-right: 1px solid #232842; overflow: auto; display: flex; flex-direction: column; gap: 6px; font-size: 13px; }
.cfd__panel h4 { margin: 10px 0 2px; color: #8b91bd; font-size: 11px; text-transform: uppercase; letter-spacing: .05em; }
.cfd__panel label { display: flex; align-items: center; gap: 6px; }
.cfd__panel select, .cfd__panel textarea { background: #151a2e; color: #e7e9f3; border: 1px solid #2b3157; border-radius: 6px; padding: 4px; }
.cfd__pad { display: grid; grid-template-columns: repeat(4, 1fr); gap: 4px; }
.cfd__rot { display: flex; flex-wrap: wrap; gap: 4px; align-items: center; }
.cfd__rot span { width: 100%; color: #8b91bd; }
.cfd__pad button, .cfd__rot button { background: #151a2e; border: 1px solid #2b3157; color: #cdd2f0; border-radius: 6px; padding: 4px 6px; cursor: pointer; }
.cfd__save { display: flex; gap: 6px; margin-top: 8px; }
.cfd__save button { flex: 1; padding: 6px; border-radius: 6px; border: 1px solid #2b3157; background: #151a2e; color: #cdd2f0; cursor: pointer; }
.cfd__save button.primary { background: #4f46e5; border-color: #4f46e5; color: #fff; }
.cfd__out { height: 150px; font-family: ui-monospace, monospace; font-size: 11px; white-space: pre; }
.cfd__hint { color: #8b91bd; font-size: 12px; line-height: 1.4; }
.cfd__stagewrap { flex: 1; display: grid; place-items: center; background:
    linear-gradient(45deg, #171b2c 25%, transparent 25%), linear-gradient(-45deg, #171b2c 25%, transparent 25%),
    linear-gradient(45deg, transparent 75%, #171b2c 75%), linear-gradient(-45deg, transparent 75%, #171b2c 75%);
  background-size: 24px 24px; background-position: 0 0, 0 12px, 12px -12px, -12px 0; }
.cfd__stage { position: relative; width: 560px; height: 560px; }
.cfd__stage.editing { cursor: grab; outline: 1px dashed #2b3157; }
.cfd__stage.editing:active { cursor: grabbing; }
.cfd__full { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: contain; }
.cfd__layer { position: absolute; inset: 0; }
.cfd__full.half, .cfd__layer.half { opacity: .5; }
.cfd__layer.bound { outline: 1px dashed rgba(120,160,255,.5); }
.cfd__layer.active { outline: 2px solid #ff3b6b; outline-offset: -1px; }
.cfd__tag { position: absolute; left: 2px; top: 2px; font-size: 9px; background: rgba(0,0,0,.6); padding: 1px 3px; border-radius: 3px; }
.cfd__pivot { position: absolute; width: 12px; height: 12px; margin: -6px 0 0 -6px; border-radius: 50%; background: #ff3b6b; box-shadow: 0 0 0 2px #fff; z-index: 200; }
.cfd__fx { position: absolute; transform: translate(-50%, -50%); object-fit: contain; mix-blend-mode: screen; pointer-events: none; }
</style>
