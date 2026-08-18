<script setup lang="ts">
/**
 * DEV-ONLY Cyber Fox 2.5D reconstruction / registration debug tool.
 * Front-facing reconstruction editor (manifest.json): drag / rotate / scale /
 * z-order / pivots + save. The Preview button plays the head/ear/tail motion
 * wave with the handles hidden. Not shipped to production (route guarded by
 * import.meta.env.DEV). The side-profile gallop rig was retired with the
 * `running` state, so this tool now only covers the front rig.
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
const clamp01 = (n: number) => Math.min(1, Math.max(0, n))

function assetUrl(rel: string): string {
  const hit = Object.keys(urlMap).find((k) => k.endsWith('/' + rel))
  return hit ? urlMap[hit] : ''
}

const preview = ref(false)

// =======================================================================
// FRONT rig (front-facing reconstruction, manifest.json) — full editor
// =======================================================================
const STACK: { key: string; file: string; z: number; kind: string; label: string; wf: number }[] = [
  { key: 'tailOrange', file: 'tail-orange.webp', z: 30, kind: 'tail', label: 'tail-orange', wf: 0.215 },
  { key: 'tailBlue', file: 'tail-blue.webp', z: 31, kind: 'tail', label: 'tail-blue', wf: 0.235 },
  { key: 'body', file: 'body.webp', z: 40, kind: 'body', label: 'body', wf: 0.42 },
  { key: 'head', file: 'head.webp', z: 50, kind: 'head', label: 'head', wf: 0.44 },
  { key: 'earLeft', file: 'ear-left.webp', z: 60, kind: 'ear', label: 'ear-left', wf: 0.15 },
  { key: 'earRight', file: 'ear-right.webp', z: 61, kind: 'ear', label: 'ear-right', wf: 0.15 },
  { key: 'eyes', file: '', z: 70, kind: 'eyes', label: 'eyes (pair)', wf: 0.07 },
  { key: 'chest', file: '', z: 80, kind: 'chest', label: 'chest', wf: 0.15 },
]
const eyeState = ref('idle')
const chestState = ref('idle')
const eyeStates = computed(() => Object.keys(manifest.eyes ?? {}))
const chestStates = computed(() => Object.keys(manifest.chest ?? {}))
const visible = reactive<Record<string, boolean>>({
  tailOrange: true, tailBlue: true, body: true, head: true,
  earLeft: true, earRight: true, eyes: true, chest: true,
})

const activePart = ref('head')
const offset = reactive<Record<string, { dx: number; dy: number; rot: number; sc: number }>>({})
const fZ = reactive<Record<string, number>>({})
STACK.forEach((s) => { offset[s.key] = { dx: 0, dy: 0, rot: 0, sc: 1 }; fZ[s.key] = s.z })

// pivots (rotate origins / hinges)
const showPivots = ref(true)
const editPivots = ref(false)
const pivotOverride = reactive<Record<string, { x: number; y: number }>>({})
function pivotOf(key: string): { x: number; y: number } | null {
  if (pivotOverride[key]) return pivotOverride[key]
  const layers = manifest.layers ?? {}
  if (layers[key]?.pivot) return layers[key].pivot
  if (key === 'tailOrange') return manifest.tails?.tailOrange?.pivot ?? null
  if (key === 'tailBlue') return manifest.tails?.tailBlue?.pivot ?? null
  return null
}
function resetPivots() { for (const k of Object.keys(pivotOverride)) delete pivotOverride[k] }
function centerOf(key: string): { x: number; y: number } {
  if (key === 'eyes') return { x: 0.5, y: placement.eyeLeft?.cy ?? 0.427 }
  const p = placement[key]
  return { x: p?.cx ?? 0.5, y: p?.cy ?? 0.5 }
}
// edit → rotate/scale around the part centre (WYSIWYG with the build script);
// preview → rotate around the hinge pivot for a natural motion wave.
function originFor(key: string): string {
  const o = preview.value ? (pivotOf(key) ?? centerOf(key)) : centerOf(key)
  return `${(o.x * 100).toFixed(2)}% ${(o.y * 100).toFixed(2)}%`
}

// z-order editing
const fByZ = computed(() => [...STACK].sort((a, b) => (fZ[a.key] ?? a.z) - (fZ[b.key] ?? b.z)))
function fZStep(dir: 1 | -1) {
  const order = fByZ.value
  const i = order.findIndex((p) => p.key === activePart.value)
  const j = i + dir
  if (i < 0 || j < 0 || j >= order.length) return
  const a = activePart.value, b = order[j].key
  const za = fZ[a], zb = fZ[b]
  fZ[a] = zb; fZ[b] = za
}
function fZFront() { fZ[activePart.value] = Math.max(...STACK.map((s) => fZ[s.key] ?? s.z)) + 2 }
function fZBack() { fZ[activePart.value] = Math.min(...STACK.map((s) => fZ[s.key] ?? s.z)) - 2 }

// drag / nudge / rotate / scale
const STAGE = 560
const stageRef = ref<HTMLElement | null>(null)
let drag: { startX: number; startY: number; base: { dx: number; dy: number }; size: number } | null = null
function onStageDown(e: PointerEvent) {
  if (preview.value) return
  const size = stageRef.value?.clientWidth ?? STAGE
  const o = offset[activePart.value]
  drag = { startX: e.clientX, startY: e.clientY, base: { dx: o.dx, dy: o.dy }, size }
  ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
}
function onStageMove(e: PointerEvent) {
  if (!drag) return
  const o = offset[activePart.value]
  o.dx = drag.base.dx + (e.clientX - drag.startX) / drag.size
  o.dy = drag.base.dy + (e.clientY - drag.startY) / drag.size
}
function onStageUp(e: PointerEvent) { drag = null; (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId) }
function nudge(axis: 'dx' | 'dy', px: number) { offset[activePart.value][axis] += px / STAGE }
function rotateBy(deg: number) { offset[activePart.value].rot += deg }
function scaleBy(f: number) { offset[activePart.value].sc = Math.max(0.2, offset[activePart.value].sc + f) }
function resetOffsets() { STACK.forEach((s) => { offset[s.key] = { dx: 0, dy: 0, rot: 0, sc: 1 }; fZ[s.key] = s.z }); resetPivots() }

// pivot drag (front stage; layers are inset:0 so % maps to stage fraction)
let pivotDrag: { key: string; el: HTMLElement } | null = null
function pivotDown(e: PointerEvent, key: string) {
  if (!editPivots.value) return
  e.stopPropagation()
  pivotDrag = { key, el: e.currentTarget as HTMLElement }
  ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
}
function pivotMove(e: PointerEvent) {
  if (!pivotDrag) return
  const rect = stageRef.value?.getBoundingClientRect()
  if (!rect) return
  pivotOverride[pivotDrag.key] = {
    x: clamp01((e.clientX - rect.left) / rect.width),
    y: clamp01((e.clientY - rect.top) / rect.height),
  }
}
function pivotUp(e: PointerEvent) {
  if (!pivotDrag) return
  pivotDrag.el.releasePointerCapture?.(e.pointerId)
  pivotDrag = null
}

// save layout
const savedText = ref('')
function num(n: number) { return Number(n).toFixed(4).replace(/0+$/, '').replace(/\.$/, '.0') }
function computeLayout() {
  const p = placement, o = offset
  const def = Object.fromEntries(STACK.map((s) => [s.key, s.wf])) as Record<string, number>
  const wf = (k: string) => (p[k]?.wf ?? def[k] ?? 0.3) * (o[k]?.sc ?? 1)
  const cx = (k: string) => (p[k]?.cx ?? 0.5) + (o[k]?.dx ?? 0)
  const cy = (k: string) => (p[k]?.cy ?? 0.5) + (o[k]?.dy ?? 0)
  const rt = (k: string) => (p[k]?.rot ?? 0) + (o[k]?.rot ?? 0)
  const L: string[] = ['// LAYOUT (paste into scripts/build-cyber-fox-layers.mjs)']
  L.push(`  body:        { wf: ${num(wf('body'))}, cx: ${num(cx('body'))}, cy: ${num(cy('body'))}, rot: ${num(rt('body'))} },`)
  L.push(`  head:        { wf: ${num(wf('head'))}, cx: ${num(cx('head'))}, cy: ${num(cy('head'))}, rot: ${num(rt('head'))} },`)
  L.push(`  'ear-left':  { wf: ${num(wf('earLeft'))}, cx: ${num(cx('earLeft'))}, cy: ${num(cy('earLeft'))}, rot: ${num(rt('earLeft'))} },`)
  L.push(`  'ear-right': { wf: ${num(wf('earRight'))}, cx: ${num(cx('earRight'))}, cy: ${num(cy('earRight'))}, rot: ${num(rt('earRight'))} },`)
  L.push(`  'tail-orange': { wf: ${num(wf('tailOrange'))}, cx: ${num(cx('tailOrange'))}, cy: ${num(cy('tailOrange'))}, rot: ${num(rt('tailOrange'))} },`)
  L.push(`  'tail-blue':   { wf: ${num(wf('tailBlue'))}, cx: ${num(cx('tailBlue'))}, cy: ${num(cy('tailBlue'))}, rot: ${num(rt('tailBlue'))} },`)
  const eo = o.eyes || { dx: 0, dy: 0, rot: 0 }
  const elx = (p.eyeLeft?.cx ?? 0.415) + eo.dx, ely = (p.eyeLeft?.cy ?? 0.427) + eo.dy
  const erx = (p.eyeRight?.cx ?? 0.585) + eo.dx, ery = (p.eyeRight?.cy ?? 0.427) + eo.dy
  L.push(`// EYE = { left: { cx: ${num(elx)}, cy: ${num(ely)} }, right: { cx: ${num(erx)}, cy: ${num(ery)} } ; EYE_ROT = ${num((p.eyeLeft?.rot ?? 0) + eo.rot)}`)
  L.push(`// CHEST_PLACE = { wf: ${num(wf('chest'))}, cx: ${num(cx('chest'))}, cy: ${num(cy('chest'))}, rot: ${num(rt('chest'))} }`)
  const zlines: string[] = ['', '// Z (z-index order)']
  const pv: string[] = ['', '// PIVOTS (rotate origins) — paste into the pivot map in scripts/build-cyber-fox-layers.mjs']
  for (const s of STACK) {
    zlines.push(`//   ${s.label}: ${fZ[s.key] ?? s.z},`)
    const pt = pivotOf(s.key)
    if (pt) pv.push(`//   ${s.label}: { x: ${num(pt.x)}, y: ${num(pt.y)} },`)
  }
  return L.join('\n') + '\n' + zlines.join('\n') + '\n' + pv.join('\n')
}
function saveLayout() {
  savedText.value = computeLayout()
  try { navigator.clipboard?.writeText(savedText.value) } catch { /* ignore */ }
  const blob = new Blob([savedText.value], { type: 'text/plain' })
  const a = document.createElement('a')
  a.href = URL.createObjectURL(blob); a.download = 'cyber-fox-layout.txt'; a.click()
  URL.revokeObjectURL(a.href)
}

function fileFor(item: { key: string; file: string; kind: string }): string {
  if (item.kind === 'eyes') return assetUrl(`eyes/${eyeState.value}.webp`)
  if (item.kind === 'chest') return assetUrl(`chest/${chestState.value}.webp`)
  return assetUrl(item.file)
}

// ---- front motion (only while previewing) ------------------------------
const amp = reactive({ head: 3, ear: 2, tail: 5 })
const t = ref(0)
let raf = 0
function loop() { t.value = (t.value + 0.016) % 1000; if (preview.value) raf = requestAnimationFrame(loop) }
function wave(speed = 1, phase = 0) { return Math.sin((t.value * speed + phase) * Math.PI * 2) }
const motionRot = computed(() => {
  const m: Record<string, number> = {}
  if (!preview.value) return m
  m.head = wave(0.5) * amp.head
  m.earLeft = (wave(0.7) - 1) / 2 * amp.ear
  m.earRight = (wave(0.7, 0.2) + 1) / 2 * amp.ear
  m.tailOrange = wave(0.4) * amp.tail
  m.tailBlue = -wave(0.4, 0.1) * amp.tail
  return m
})
function transformOf(key: string): string {
  const o = offset[key] || { dx: 0, dy: 0, rot: 0, sc: 1 }
  const move = (o.dx || o.dy) ? `translate(${(o.dx * 100).toFixed(3)}%, ${(o.dy * 100).toFixed(3)}%)` : ''
  const scl = (o.sc && o.sc !== 1) ? ` scale(${o.sc.toFixed(3)})` : ''
  const rotDeg = o.rot + (preview.value ? (motionRot.value[key] ?? 0) : 0)
  const rot = rotDeg ? ` rotate(${rotDeg.toFixed(2)}deg)` : ''
  return `${move}${rot}${scl}` || 'none'
}

// ---- preview control ---------------------------------------------------
function togglePreview() {
  if (preview.value) { stopPreview(); return }
  preview.value = true
  t.value = 0; raf = requestAnimationFrame(loop)
}
function stopPreview() { preview.value = false; cancelAnimationFrame(raf) }
</script>

<template>
  <div class="cfd">
    <header class="cfd__bar">
      <strong>Cyber Fox 2.5D · Debug (front rig)</strong>
      <button class="cfd__preview" :class="{ on: preview }" @click="togglePreview">
        {{ preview ? '■ Stop preview' : '▶ Preview (animation)' }}
      </button>
    </header>

    <div class="cfd__body">
      <aside class="cfd__panel">
        <h4>Active part</h4>
        <select v-model="activePart" :disabled="preview">
          <option v-for="s in STACK" :key="s.key" :value="s.key">{{ s.label }}</option>
        </select>
        <template v-if="!preview">
          <h4>Move (1px)</h4>
          <div class="cfd__pad">
            <button @click="nudge('dy', -1)">↑</button>
            <button @click="nudge('dx', -1)">←</button>
            <button @click="nudge('dx', 1)">→</button>
            <button @click="nudge('dy', 1)">↓</button>
          </div>
          <h4>Rotate / Scale</h4>
          <div class="cfd__rot">
            <button @click="rotateBy(-1)">↺ -1°</button>
            <button @click="rotateBy(1)">↻ +1°</button>
            <button @click="scaleBy(-0.02)">− scale</button>
            <button @click="scaleBy(0.02)">+ scale</button>
            <span>rot {{ offset[activePart].rot.toFixed(1) }}° · scale {{ offset[activePart].sc.toFixed(2) }}</span>
          </div>
          <h4>Z-order (front / back)</h4>
          <div class="cfd__rot">
            <button @click="fZStep(1)">⬆ forward</button>
            <button @click="fZStep(-1)">⬇ backward</button>
            <button @click="fZFront()">⤒ to front</button>
            <button @click="fZBack()">⤓ to back</button>
            <span>z: {{ fZ[activePart] }}</span>
          </div>
          <div class="cfd__zlist">
            <div v-for="s in [...fByZ].reverse()" :key="'fzl' + s.key" class="cfd__zrow" :class="{ on: activePart === s.key, off: !visible[s.key] }">
              <button class="cfd__eye" :title="visible[s.key] ? 'Hide' : 'Show'" @click="visible[s.key] = !visible[s.key]">{{ visible[s.key] ? '👁' : '🚫' }}</button>
              <span @click="activePart = s.key">{{ s.label }} ({{ fZ[s.key] }})</span>
            </div>
          </div>
          <h4>Eyes / Chest</h4>
          <select v-model="eyeState"><option v-for="e in eyeStates" :key="e" :value="e">eyes: {{ e }}</option></select>
          <select v-model="chestState"><option v-for="c in chestStates" :key="c" :value="c">chest: {{ c }}</option></select>
          <h4>Pivots (rotate origins)</h4>
          <label><input type="checkbox" v-model="showPivots" /> show pivots</label>
          <label><input type="checkbox" v-model="editPivots" /> edit pivots (drag dots)</label>
          <div class="cfd__rot" v-if="pivotOf(activePart)">
            <span>pivot: x {{ pivotOf(activePart)!.x.toFixed(3) }} · y {{ pivotOf(activePart)!.y.toFixed(3) }}</span>
            <button @click="resetPivots">Reset pivots</button>
          </div>
          <div class="cfd__save">
            <button class="primary" @click="saveLayout">💾 Save layout</button>
            <button @click="resetOffsets">Reset</button>
          </div>
          <textarea v-if="savedText" class="cfd__out" readonly :value="savedText"></textarea>
          <p class="cfd__hint">Chọn part → kéo trên stage để dời, nút để xoay/scale/z-order. Bật <b>edit pivots</b> để kéo chấm hồng chỉnh điểm xoay. Save → copy + tải <code>cyber-fox-layout.txt</code>.</p>
        </template>
        <template v-else>
          <h4>Preview motion (amplitude)</h4>
          <label>head ±{{ amp.head }}°<input type="range" min="0" max="8" step="0.5" v-model.number="amp.head" /></label>
          <label>ear ±{{ amp.ear }}°<input type="range" min="0" max="6" step="0.5" v-model.number="amp.ear" /></label>
          <label>tail ±{{ amp.tail }}°<input type="range" min="0" max="10" step="0.5" v-model.number="amp.tail" /></label>
          <p class="cfd__hint">Motion wave head/ear/tail xoay quanh pivot. Tắt preview để chỉnh tiếp.</p>
        </template>
      </aside>

      <main class="cfd__stagewrap">
        <div ref="stageRef" class="cfd__stage" :class="{ editing: !preview }"
             @pointerdown="onStageDown" @pointermove="(e) => { onStageMove(e); pivotMove(e) }"
             @pointerup="(e) => { onStageUp(e); pivotUp(e) }" @pointercancel="(e) => { onStageUp(e); pivotUp(e) }">
          <div v-for="s in STACK" :key="s.key" v-show="visible[s.key]" class="cfd__layer"
               :class="{ active: !preview && activePart === s.key }"
               :style="{ zIndex: fZ[s.key] ?? s.z, transformOrigin: originFor(s.key), transform: transformOf(s.key) }">
            <img class="cfd__full" :src="fileFor(s)" alt="" draggable="false" />
            <span v-if="!preview && activePart === s.key" class="cfd__tag">{{ s.label }}</span>
          </div>
          <template v-if="showPivots && !preview">
            <span v-for="s in STACK" :key="'fp' + s.key" v-show="pivotOf(s.key) && visible[s.key]" class="cfd__pivot"
                  :class="{ draggable: editPivots, active: activePart === s.key }"
                  @pointerdown="(e) => { activePart = s.key; pivotDown(e, s.key) }"
                  :style="{ left: ((pivotOf(s.key)?.x ?? 0.5) * 100) + '%', top: ((pivotOf(s.key)?.y ?? 0.5) * 100) + '%', zIndex: 300 }" />
          </template>
        </div>
      </main>
    </div>
  </div>
</template>

<style scoped>
.cfd { height: 100%; display: flex; flex-direction: column; background: #0b0d17; color: #e7e9f3; }
.cfd__bar { display: flex; align-items: center; gap: 16px; padding: 10px 16px; border-bottom: 1px solid #232842; }
.cfd__preview { margin-left: auto; padding: 5px 14px; border-radius: 6px; border: 1px solid #2b3157; background: #151a2e; color: #cdd2f0; cursor: pointer; font-size: 13px; }
.cfd__preview.on { background: #16a34a; border-color: #16a34a; color: #fff; }
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
.cfd__zlist { display: flex; flex-direction: column; gap: 2px; margin: 4px 0; }
.cfd__zrow { display: flex; align-items: center; gap: 4px; font-size: 11px; padding: 1px 4px; border-radius: 4px; background: #151a2e; border: 1px solid transparent; }
.cfd__zrow > span { flex: 1; color: #b7bde0; cursor: pointer; padding: 1px 2px; }
.cfd__zrow.on { border-color: #ff3b6b; }
.cfd__zrow.on > span { color: #fff; }
.cfd__zrow.off > span { color: #55597a; text-decoration: line-through; }
.cfd__eye { background: none; border: none; cursor: pointer; font-size: 11px; line-height: 1; padding: 0; filter: grayscale(0); }
.cfd__zrow.off .cfd__eye { filter: grayscale(1) opacity(0.7); }
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
.cfd__layer.active { outline: 2px solid #ff3b6b; outline-offset: -1px; }
.cfd__tag { position: absolute; left: 2px; top: 2px; font-size: 9px; background: rgba(0,0,0,.6); padding: 1px 3px; border-radius: 3px; }
.cfd__pivot { position: absolute; width: 12px; height: 12px; margin: -6px 0 0 -6px; border-radius: 50%; background: #ff3b6b; box-shadow: 0 0 0 2px #fff; z-index: 200; }
.cfd__pivot.draggable { cursor: grab; width: 16px; height: 16px; margin: -8px 0 0 -8px; touch-action: none; }
.cfd__pivot.draggable:active { cursor: grabbing; }
.cfd__pivot.active { background: #ffd23b; box-shadow: 0 0 0 2px #fff, 0 0 0 4px rgba(255,210,59,.4); }
</style>
