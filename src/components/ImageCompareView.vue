<script setup lang="ts">
// Side-by-side image comparison with a SINGLE shared zoom/pan state: zooming or
// dragging either pane transforms both images identically, so pixels line up for
// visual diffing. Wheel (Ctrl/Cmd) zooms, drag pans, double-click toggles zoom.
import { ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ZoomIn, ZoomOut, ArrowLeftRight, X } from 'lucide-vue-next'
import { convertFileSrc } from '@tauri-apps/api/core'

const props = defineProps<{
  projectPath: string
  left: string
  right: string
}>()

const emit = defineEmits<{ close: []; swap: [] }>()

const { t } = useI18n()

function basename(p: string): string {
  return p.split('/').pop() ?? p
}

// Resolve a project-relative path to a cache-busted asset URL (the asset://
// protocol caches by path, so a changed image would otherwise stay stale).
function mediaUrl(rel: string): string {
  const root = props.projectPath.replace(/\/+$/, '')
  const abs = rel.startsWith('/') ? rel : `${root}/${rel}`
  return `${convertFileSrc(abs)}?v=${Date.now()}`
}

const leftUrl = computed(() => mediaUrl(props.left))
const rightUrl = computed(() => mediaUrl(props.right))

// ── Shared zoom / pan ────────────────────────────────────────────────────────
const ZOOM_MIN = 0.1
const ZOOM_MAX = 8
const zoom = ref(1)
const panX = ref(0)
const panY = ref(0)
const panning = ref(false)
let panStartX = 0
let panStartY = 0
let panOriginX = 0
let panOriginY = 0

const imageStyle = computed(() => ({
  transform: `translate(${panX.value}px, ${panY.value}px) scale(${zoom.value})`,
  cursor: zoom.value > 1 ? (panning.value ? 'grabbing' : 'grab') : 'default',
}))

function clampZoom(v: number): number {
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, v))
}
function setZoom(next: number) {
  const z = clampZoom(next)
  if (z === zoom.value) return
  if (z <= 1) { panX.value = 0; panY.value = 0 }
  zoom.value = z
}
function zoomIn() { setZoom(zoom.value * 1.25) }
function zoomOut() { setZoom(zoom.value / 1.25) }
function resetZoom() { zoom.value = 1; panX.value = 0; panY.value = 0 }

function onWheel(e: WheelEvent) {
  if (!(e.ctrlKey || e.metaKey)) return
  e.preventDefault()
  setZoom(zoom.value * (e.deltaY < 0 ? 1.1 : 1 / 1.1))
}
function onPanStart(e: PointerEvent) {
  if (zoom.value <= 1 || e.button !== 0) return
  e.preventDefault()
  panning.value = true
  panStartX = e.clientX
  panStartY = e.clientY
  panOriginX = panX.value
  panOriginY = panY.value
  ;(e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId)
}
function onPanMove(e: PointerEvent) {
  if (!panning.value) return
  panX.value = panOriginX + (e.clientX - panStartX)
  panY.value = panOriginY + (e.clientY - panStartY)
}
function onPanEnd(e: PointerEvent) {
  if (!panning.value) return
  panning.value = false
  ;(e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId)
}
function onDblClick() {
  if (zoom.value > 1) resetZoom()
  else setZoom(2)
}
</script>

<template>
  <div class="flex flex-col h-full min-h-0 bg-card select-none">
    <!-- Header -->
    <div class="flex items-center gap-2 border-b border-border bg-card px-4 py-2.5 shrink-0">
      <span class="text-xs font-semibold text-foreground/90 shrink-0">{{ t('files.compare.title') }}</span>
      <div class="flex-1 min-w-0 flex items-center gap-2 text-[11px] font-mono text-foreground/60">
        <span class="truncate flex-1 text-right" :title="left">{{ basename(left) }}</span>
        <span class="text-foreground/30 shrink-0">↔</span>
        <span class="truncate flex-1" :title="right">{{ basename(right) }}</span>
      </div>

      <!-- Zoom controls (shared) -->
      <div class="flex items-center rounded-md border border-border overflow-hidden shrink-0">
        <button
          class="flex items-center justify-center h-6 w-6 text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-40 disabled:cursor-default"
          :title="t('files.viewer.zoomOut')"
          :disabled="zoom <= ZOOM_MIN"
          @click="zoomOut"
        >
          <ZoomOut class="h-3.5 w-3.5" :stroke-width="1.75" />
        </button>
        <button
          class="px-1.5 h-6 text-[10px] tabular-nums text-foreground/60 hover:text-foreground border-x border-border select-none cursor-pointer min-w-[3rem]"
          :title="t('files.viewer.resetZoom')"
          @click="resetZoom"
        >{{ Math.round(zoom * 100) }}%</button>
        <button
          class="flex items-center justify-center h-6 w-6 text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-40 disabled:cursor-default"
          :title="t('files.viewer.zoomIn')"
          :disabled="zoom >= ZOOM_MAX"
          @click="zoomIn"
        >
          <ZoomIn class="h-3.5 w-3.5" :stroke-width="1.75" />
        </button>
      </div>

      <!-- Swap sides -->
      <button
        class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
        :title="t('files.compare.swap')"
        @click="emit('swap')"
      >
        <ArrowLeftRight class="h-3.5 w-3.5" :stroke-width="1.75" />
      </button>
      <!-- Close -->
      <button
        class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
        :title="t('common.close')"
        @click="emit('close')"
      >
        <X class="h-4 w-4" :stroke-width="1.75" />
      </button>
    </div>

    <!-- Panes -->
    <div class="flex-1 min-h-0 flex" @wheel="onWheel">
      <div
        class="relative flex-1 min-w-0 flex items-center justify-center overflow-hidden bg-foreground/5"
      >
        <img
          :src="leftUrl"
          :alt="left"
          class="max-w-full max-h-full object-contain will-change-transform"
          :class="{ 'transition-transform duration-75': !panning }"
          :style="imageStyle"
          draggable="false"
          @pointerdown="onPanStart"
          @pointermove="onPanMove"
          @pointerup="onPanEnd"
          @pointercancel="onPanEnd"
          @dblclick="onDblClick"
        />
      </div>
      <div class="w-px bg-border shrink-0" />
      <div
        class="relative flex-1 min-w-0 flex items-center justify-center overflow-hidden bg-foreground/5"
      >
        <img
          :src="rightUrl"
          :alt="right"
          class="max-w-full max-h-full object-contain will-change-transform"
          :class="{ 'transition-transform duration-75': !panning }"
          :style="imageStyle"
          draggable="false"
          @pointerdown="onPanStart"
          @pointermove="onPanMove"
          @pointerup="onPanEnd"
          @pointercancel="onPanEnd"
          @dblclick="onDblClick"
        />
      </div>
    </div>

    <!-- Hint -->
    <div class="shrink-0 border-t border-border px-4 py-1.5 text-[10px] text-foreground/40 text-center">
      {{ t('files.compare.hint') }}
    </div>
  </div>
</template>
