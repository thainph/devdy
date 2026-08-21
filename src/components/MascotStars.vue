<script setup lang="ts">
/**
 * Ascension stars ⭐ for the Cyber Fox. One star per completed 35-level cycle
 * ("chuyển sinh"). Renders nothing on the first cycle (count = 0). Purely
 * decorative — kept as a DOM overlay so it works over the canvas-rendered fox
 * on every surface (floating mascot, desktop pet, Settings preview).
 */
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    count?: number
    /** Glyph size in px. */
    size?: number
    /** Accessible label describing the ascension count. */
    label?: string
  }>(),
  { count: 0, size: 14, label: '' },
)

const stars = computed(() => Math.max(0, Math.floor(props.count || 0)))
</script>

<template>
  <div
    v-if="stars > 0"
    class="mascot-stars"
    :style="{ '--star-size': `${size}px` }"
    :aria-label="label || undefined"
    :role="label ? 'img' : undefined"
    :aria-hidden="label ? undefined : 'true'"
  >
    <span
      v-for="n in stars"
      :key="n"
      class="mascot-stars__star"
      :style="{ animationDelay: `${(n - 1) * 160}ms` }"
      >★</span
    >
  </div>
</template>

<style scoped>
.mascot-stars {
  display: inline-flex;
  align-items: center;
  gap: 1px;
  pointer-events: none;
  user-select: none;
  line-height: 1;
}
.mascot-stars__star {
  font-size: var(--star-size, 14px);
  color: #ffd76a;
  text-shadow:
    0 0 4px rgba(255, 200, 80, 0.9),
    0 0 10px rgba(255, 160, 40, 0.5);
  animation: mascot-star-twinkle 2400ms ease-in-out infinite;
}
@keyframes mascot-star-twinkle {
  0%,
  100% {
    transform: translateY(0) scale(1);
    opacity: 0.9;
  }
  50% {
    transform: translateY(-1px) scale(1.12);
    opacity: 1;
  }
}
@media (prefers-reduced-motion: reduce) {
  .mascot-stars__star {
    animation: none;
  }
}
</style>
