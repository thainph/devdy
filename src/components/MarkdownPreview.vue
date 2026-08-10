<script setup lang="ts">
// Compact, height-clamped markdown preview for list rows. Renders pre-computed
// markdown HTML capped at `maxHeight`; when the content overflows it fades the
// clipped edge to hint there's more — the parent row opens the full detail in a
// drawer on click. Overflow is measured from scrollHeight so it works while
// clipped.
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'

const props = withDefaults(defineProps<{
  html: string
  /** Max height (px) before the content is clipped and the fade appears. */
  maxHeight?: number
}>(), {
  maxHeight: 104,
})

const el = ref<HTMLElement | null>(null)
const overflowing = ref(false)

function measure() {
  const node = el.value
  overflowing.value = node ? node.scrollHeight > props.maxHeight + 8 : false
}

let ro: ResizeObserver | null = null
onMounted(() => {
  measure()
  ro = new ResizeObserver(() => measure())
  if (el.value) ro.observe(el.value)
})
onBeforeUnmount(() => ro?.disconnect())
watch(() => props.html, () => requestAnimationFrame(measure))

defineExpose({ overflowing })
</script>

<template>
  <div class="relative">
    <div
      ref="el"
      class="markdown-output text-sm"
      :style="{ maxHeight: `${maxHeight}px`, overflow: 'hidden' }"
      v-html="html"
    />
    <div
      v-if="overflowing"
      class="pointer-events-none absolute inset-x-0 bottom-0 h-8 bg-gradient-to-t from-card to-transparent"
    />
  </div>
</template>
