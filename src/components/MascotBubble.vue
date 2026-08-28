<script setup lang="ts">
// Speech bubble shown above the DY Cyber Fox. Fully self-managed: it appears
// when a new message arrives, auto-hides after the message's duration, pauses
// while hovered, and can be dismissed with the × button. Positioned relative to
// the nearest positioned ancestor (the fox container), so both the in-app and
// desktop-pet surfaces just drop it inside the fox wrapper.
import { onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { X } from 'lucide-vue-next'
import type { MascotBubbleMessage } from '@/composables/useMascotBubble'

const props = defineProps<{
  message: MascotBubbleMessage | null
  /** Where the bubble sits relative to the fox. 'bottom' lets the desktop pet be
   *  dragged to the very top of the screen (the fox goes above, bubble below). */
  placement?: 'top' | 'bottom'
}>()

const { t } = useI18n()

const visible = ref(false)
const shown = ref<MascotBubbleMessage | null>(null)
// Text revealed so far by the typewriter effect + whether it's still typing.
const typed = ref('')
const isTyping = ref(false)
let hideTimer: ReturnType<typeof setTimeout> | null = null
let typeTimer: ReturnType<typeof setTimeout> | null = null
let hovered = false

// Typewriter tuning: ~26ms/char, but the whole line never takes longer than
// TYPE_MAX so long messages still finish promptly.
const TYPE_SPEED = 26
const TYPE_MAX = 2200

function prefersReducedMotion(): boolean {
  return (
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  )
}


function clearTimer() {
  if (hideTimer) {
    clearTimeout(hideTimer)
    hideTimer = null
  }
}

function stopTyping() {
  if (typeTimer) {
    clearTimeout(typeTimer)
    typeTimer = null
  }
  isTyping.value = false
}

function armTimer(duration: number) {
  clearTimer()
  if (duration > 0) hideTimer = setTimeout(() => { if (!hovered) hide() }, duration)
}

function hide() {
  clearTimer()
  stopTyping()
  visible.value = false
}

// Reveal the message one character at a time so the fox looks like it's talking.
// The auto-hide timer only arms once typing finishes, so text is never cut off.
function startTyping(msg: MascotBubbleMessage) {
  stopTyping()
  const full = msg.text ?? ''
  if (prefersReducedMotion() || full.length <= 1) {
    typed.value = full
    if (!hovered) armTimer(msg.duration)
    return
  }
  typed.value = ''
  isTyping.value = true
  const per = Math.max(8, Math.min(TYPE_SPEED, Math.floor(TYPE_MAX / full.length)))
  let i = 0
  const step = () => {
    i += 1
    typed.value = full.slice(0, i)
    if (i < full.length) {
      typeTimer = setTimeout(step, per)
    } else {
      typeTimer = null
      isTyping.value = false
      if (!hovered) armTimer(msg.duration)
    }
  }
  typeTimer = setTimeout(step, per)
}

function onEnter() {
  hovered = true
  clearTimer() // keep it up while the user reads / interacts
}

function onLeave() {
  hovered = false
  // Give a brief grace period, then hide — but not while still typing (the
  // typewriter arms its own hide timer once it's done).
  if (!isTyping.value) armTimer(1200)
}

watch(
  () => props.message?.id,
  (id) => {
    if (!id || !props.message) return
    shown.value = props.message
    visible.value = true
    startTyping(props.message)
  },
)

onBeforeUnmount(() => {
  clearTimer()
  stopTyping()
})
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
      :class="{ 'is-bottom': placement === 'bottom' }"
      @pointerdown.stop
      @mouseenter="onEnter"
      @mouseleave="onLeave"
    >
      <div class="mascot-bubble-box" role="status">
        <span class="mascot-bubble-text"
          >{{ typed }}<span
            v-if="isTyping"
            class="mascot-bubble-caret"
            aria-hidden="true"
          /></span>
        <button
          type="button"
          class="mascot-bubble-close"
          :aria-label="t('mascot.bubble.dismiss')"
          :title="t('mascot.bubble.dismiss')"
          @click="hide"
        >
          <X class="h-3 w-3" :stroke-width="2.25" />
        </button>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.mascot-bubble-wrap {
  position: absolute;
  bottom: calc(100% + 20px);
  left: 50%;
  transform: translateX(-50%);
  z-index: 5;
  pointer-events: auto;
  /* Cap to the fox column but never overflow a narrow desktop-pet window. */
  width: max-content;
  max-width: min(300px, 84vw);
  /* One soft drop-shadow wraps the whole silhouette (box + tail) as a unit. */
  filter:
    drop-shadow(0 3px 6px rgb(0 0 0 / 0.30))
    drop-shadow(0 14px 26px rgb(0 0 0 / 0.38));
}

/* Bubble placed BELOW the fox (desktop pet near the top of the screen). */
.mascot-bubble-wrap.is-bottom {
  bottom: auto;
  top: calc(100% + 20px);
}
/* Flip the tail to point UP at the fox above. */
.mascot-bubble-wrap.is-bottom .mascot-bubble-box::after {
  bottom: auto;
  top: -7px;
  border-right: none;
  border-bottom: none;
  border-left: 1px solid hsl(var(--border) / 0.35);
  border-top: 1px solid hsl(var(--border) / 0.35);
  border-bottom-right-radius: 0;
  border-top-left-radius: 5px;
}
/* Move the luminous hairline to the bottom edge (nearest the tail). */
.mascot-bubble-wrap.is-bottom .mascot-bubble-box::before {
  top: auto;
  bottom: 0;
}

.mascot-bubble-box {
  /* Single fixed accent colour for every state (glow + hairline + caret). */
  --bub: var(--primary);
  position: relative;
  display: flex;
  align-items: flex-start;
  gap: 9px;
  padding: 11px 14px 12px 15px;
  /* Big, even rounding = friendly chat-bubble pill. */
  border-radius: 20px;
  /* Soft, low-contrast rim. */
  border: 1px solid hsl(var(--border) / 0.35);
  /* Near-opaque surface so it reads as a solid bubble, with a gentle
     top→bottom sheen for a little volume. */
  background:
    linear-gradient(
      180deg,
      hsl(var(--popover) / 0.99) 0%,
      hsl(var(--popover) / 0.97) 100%
    );
  -webkit-backdrop-filter: blur(12px) saturate(1.3);
  backdrop-filter: blur(12px) saturate(1.3);
  color: hsl(var(--foreground));
  font-size: 12.5px;
  line-height: 1.42;
  /* Bright inner top edge for a subtle bevel + a faint coloured aura. */
  box-shadow:
    inset 0 1px 0 hsl(0 0% 100% / 0.12),
    0 0 22px -8px hsl(var(--bub) / 0.35);
}

/* Thin luminous hairline along the top edge — the "cyber" cue. */
.mascot-bubble-box::before {
  content: "";
  position: absolute;
  inset: 0 14px auto 14px;
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

/* Chat-bubble tail: a rotated square growing out of the bottom edge, pointing
   down at the fox. It shares the body's fill and carries the rim only on its two
   outer (down-facing) edges, so it reads as one continuous silhouette. It sits
   on top of the box's bottom border, hiding the seam. Offset slightly left of
   centre for a natural, hand-drawn feel. */
.mascot-bubble-box::after {
  content: "";
  position: absolute;
  left: 42%;
  bottom: -7px;
  width: 16px;
  height: 16px;
  transform: translateX(-50%) rotate(45deg);
  background: hsl(var(--popover) / 0.98);
  border-right: 1px solid hsl(var(--border) / 0.35);
  border-bottom: 1px solid hsl(var(--border) / 0.35);
  /* Round the outer tip so the tail curves instead of ending in a sharp point. */
  border-bottom-right-radius: 5px;
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

/* Blinking caret shown while the typewriter is revealing the text. */
.mascot-bubble-caret {
  display: inline-block;
  width: 2px;
  height: 1em;
  margin-left: 1px;
  vertical-align: -0.15em;
  border-radius: 1px;
  background: hsl(var(--bub));
  animation: mascot-bubble-caret 900ms steps(2, start) infinite;
}

@keyframes mascot-bubble-caret {
  0%, 100% { opacity: 1; }
  50% { opacity: 0; }
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

@media (prefers-reduced-motion: reduce) {
  .mascot-bubble-caret {
    animation: none;
    opacity: 0;
  }
}
</style>
