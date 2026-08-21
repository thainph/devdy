<script setup lang="ts">
// Speech bubble shown above the DY Cyber Fox. Fully self-managed: it appears
// when a new message arrives, auto-hides after the message's duration, pauses
// while hovered, and can be dismissed with the × button. Positioned relative to
// the nearest positioned ancestor (the fox container), so both the in-app and
// desktop-pet surfaces just drop it inside the fox wrapper.
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Info, CheckCircle2, AlertCircle, Loader2, ShieldAlert, X } from 'lucide-vue-next'
import type { Component } from 'vue'
import type { MascotBubbleMessage, MascotBubbleVariant } from '@/composables/useMascotBubble'

const props = defineProps<{ message: MascotBubbleMessage | null }>()

const { t } = useI18n()

const visible = ref(false)
const shown = ref<MascotBubbleMessage | null>(null)
let hideTimer: ReturnType<typeof setTimeout> | null = null
let hovered = false

const ICONS: Record<MascotBubbleVariant, Component> = {
  info: Info,
  success: CheckCircle2,
  error: AlertCircle,
  thinking: Loader2,
  permission: ShieldAlert,
}

const icon = computed<Component>(() => ICONS[shown.value?.variant ?? 'info'])
const variantClass = computed(() => `mascot-bubble-box--${shown.value?.variant ?? 'info'}`)
const spinning = computed(() => shown.value?.variant === 'thinking')

function clearTimer() {
  if (hideTimer) {
    clearTimeout(hideTimer)
    hideTimer = null
  }
}

function armTimer(duration: number) {
  clearTimer()
  if (duration > 0) hideTimer = setTimeout(() => { if (!hovered) hide() }, duration)
}

function hide() {
  clearTimer()
  visible.value = false
}

function onEnter() {
  hovered = true
  clearTimer() // keep it up while the user reads / interacts
}

function onLeave() {
  hovered = false
  // Give a brief grace period, then hide.
  armTimer(1200)
}

watch(
  () => props.message?.id,
  (id) => {
    if (!id || !props.message) return
    shown.value = props.message
    visible.value = true
    if (!hovered) armTimer(props.message.duration)
  },
)

onBeforeUnmount(clearTimer)
</script>

<template>
  <Transition
    enter-active-class="transition duration-200 ease-out"
    enter-from-class="opacity-0 translate-y-1 scale-95"
    enter-to-class="opacity-100 translate-y-0 scale-100"
    leave-active-class="transition duration-150 ease-in"
    leave-from-class="opacity-100 translate-y-0 scale-100"
    leave-to-class="opacity-0 translate-y-1 scale-95"
  >
    <div
      v-if="visible && shown"
      class="mascot-bubble-wrap"
      @pointerdown.stop
      @mouseenter="onEnter"
      @mouseleave="onLeave"
    >
      <div class="mascot-bubble-box" :class="variantClass" role="status">
        <span class="mascot-bubble-chip">
          <component
            :is="icon"
            class="mascot-bubble-icon"
            :class="{ 'mascot-bubble-icon--spin': spinning }"
            :stroke-width="2"
          />
        </span>
        <span class="mascot-bubble-text">{{ shown.text }}</span>
        <button
          type="button"
          class="mascot-bubble-close"
          :aria-label="t('mascot.bubble.dismiss')"
          :title="t('mascot.bubble.dismiss')"
          @click="hide"
        >
          <X class="h-3 w-3" :stroke-width="2.25" />
        </button>
        <!-- Trailing dots that bridge the bubble down to the fox's head. -->
        <span class="mascot-bubble-connector" aria-hidden="true">
          <span class="mascot-bubble-dot mascot-bubble-dot--1" />
          <span class="mascot-bubble-dot mascot-bubble-dot--2" />
        </span>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.mascot-bubble-wrap {
  position: absolute;
  bottom: calc(100% + 18px);
  left: 50%;
  transform: translateX(-50%);
  z-index: 5;
  pointer-events: auto;
  /* Cap to the fox column but never overflow a narrow desktop-pet window. */
  width: max-content;
  max-width: min(300px, 84vw);
  /* Two stacked drop-shadows wrap the whole silhouette (box + tail) for depth. */
  filter:
    drop-shadow(0 2px 4px rgb(0 0 0 / 0.28))
    drop-shadow(0 16px 30px rgb(0 0 0 / 0.42));
}

.mascot-bubble-box {
  /* Per-variant accent colour (HSL triplet) consumed by the chip + glow. */
  --bub: var(--primary);
  position: relative;
  display: flex;
  align-items: flex-start;
  gap: 9px;
  padding: 10px 12px 11px 10px;
  border-radius: 18px;
  /* Soft, low-contrast rim instead of a hard 1px line. */
  border: 1px solid hsl(var(--border) / 0.28);
  /* Stronger top→bottom gradient reads as a lit, convex surface. */
  background:
    linear-gradient(
      177deg,
      hsl(var(--popover) / 0.98) 0%,
      hsl(var(--popover) / 0.9) 55%,
      hsl(var(--popover) / 0.82) 100%
    );
  -webkit-backdrop-filter: blur(14px) saturate(1.35);
  backdrop-filter: blur(14px) saturate(1.35);
  color: hsl(var(--foreground));
  font-size: 12.5px;
  line-height: 1.42;
  /* Bevel: bright inner top edge + soft inner bottom shade = rounded volume;
     plus a faint coloured aura so it glows rather than sits flat. */
  box-shadow:
    inset 0 1px 0 hsl(0 0% 100% / 0.14),
    inset 0 6px 14px -10px hsl(0 0% 100% / 0.16),
    inset 0 -14px 20px -14px hsl(240 55% 2% / 0.55),
    0 0 24px -6px hsl(var(--bub) / 0.4);
}

/* Thin luminous hairline along the top edge — the "cyber" cue. */
.mascot-bubble-box::before {
  content: "";
  position: absolute;
  inset: 0 12px auto 12px;
  top: 0;
  height: 1px;
  border-radius: 999px;
  background: linear-gradient(
    90deg,
    transparent,
    hsl(var(--bub) / 0.55),
    transparent
  );
}

.mascot-bubble-box--success { --bub: 142 70% 45%; }
.mascot-bubble-box--error { --bub: 0 72% 58%; }
.mascot-bubble-box--permission { --bub: 38 92% 55%; }
.mascot-bubble-box--thinking { --bub: var(--primary); }
.mascot-bubble-box--info { --bub: var(--primary); }

/* Glowing icon chip. */
.mascot-bubble-chip {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  flex-shrink: 0;
  margin-top: 1px;
  border-radius: 8px;
  background: linear-gradient(180deg, hsl(var(--bub) / 0.22), hsl(var(--bub) / 0.12));
  box-shadow:
    inset 0 1px 0 hsl(0 0% 100% / 0.18),
    inset 0 0 0 1px hsl(var(--bub) / 0.3),
    0 0 12px -4px hsl(var(--bub) / 0.5);
}
.mascot-bubble-icon {
  width: 14px;
  height: 14px;
  color: hsl(var(--bub));
}
.mascot-bubble-icon--spin {
  animation: mascot-bubble-spin 1s linear infinite;
}

.mascot-bubble-text {
  flex: 1;
  min-width: 0;
  padding-top: 3px;
  font-weight: 450;
  color: hsl(var(--foreground) / 0.92);
  /* Wrap by words; only break a word if it alone is wider than the bubble. */
  white-space: normal;
  word-break: normal;
  overflow-wrap: anywhere;
  /* Cap runaway messages at ~4 lines. */
  display: -webkit-box;
  -webkit-line-clamp: 4;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.mascot-bubble-close {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  flex-shrink: 0;
  margin-top: 2px;
  border-radius: 999px;
  color: hsl(var(--muted-foreground));
  cursor: pointer;
  transition: background-color 120ms ease, color 120ms ease;
}
.mascot-bubble-close:hover {
  background: hsl(var(--accent));
  color: hsl(var(--foreground));
}

/* Connector: two shrinking glass beads that trail from the bubble down toward
   the fox's head, so the pairing reads as one organic "the fox is speaking"
   unit instead of a bubble with a detached pointer. */
.mascot-bubble-connector {
  position: absolute;
  top: calc(100% - 2px);
  left: 50%;
  transform: translateX(-50%);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 3px;
  padding-top: 3px;
}
.mascot-bubble-dot {
  border-radius: 999px;
  background: linear-gradient(180deg, hsl(var(--popover) / 0.97), hsl(var(--popover) / 0.84));
  border: 1px solid hsl(var(--border) / 0.25);
  box-shadow:
    inset 0 1px 0 hsl(0 0% 100% / 0.16),
    0 0 10px -4px hsl(var(--bub) / 0.55);
}
.mascot-bubble-dot--1 {
  width: 9px;
  height: 9px;
}
.mascot-bubble-dot--2 {
  width: 5.5px;
  height: 5.5px;
}

@keyframes mascot-bubble-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .mascot-bubble-icon--spin {
    animation: none;
  }
}
</style>
