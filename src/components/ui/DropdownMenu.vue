<script setup lang="ts">
// Lightweight, on-brand dropdown. Provide the trigger via #trigger slot and
// menu items (DropdownItem) in the default slot. Closes on outside click,
// Escape, or after an item is clicked. Flips above the trigger automatically
// when there isn't enough room below (e.g. rows near the bottom of a scroll
// list). Emits `update:open` so callers can keep the trigger visible while open.
import { ref, onMounted, onBeforeUnmount } from 'vue'

withDefaults(defineProps<{ align?: 'left' | 'right' }>(), { align: 'right' })
const emit = defineEmits<{ (e: 'update:open', open: boolean): void }>()

const open = ref(false)
const flipUp = ref(false)
const root = ref<HTMLElement | null>(null)

function setOpen(v: boolean) {
  if (open.value === v) return
  open.value = v
  emit('update:open', v)
}
function toggle() {
  const next = !open.value
  // Decide direction before opening: flip up only when there's clearly more
  // room above than below (keeps the menu on-screen inside scroll containers).
  if (next && root.value) {
    const rect = root.value.getBoundingClientRect()
    const spaceBelow = window.innerHeight - rect.bottom
    flipUp.value = spaceBelow < 260 && rect.top > spaceBelow
  }
  setOpen(next)
}
function close() { setOpen(false) }
function onDoc(e: MouseEvent) {
  if (root.value && !root.value.contains(e.target as Node)) close()
}
function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') close()
}
onMounted(() => {
  document.addEventListener('mousedown', onDoc)
  document.addEventListener('keydown', onKey)
})
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onDoc)
  document.removeEventListener('keydown', onKey)
})
</script>

<template>
  <div ref="root" class="relative inline-flex">
    <span class="inline-flex" @click="toggle">
      <slot name="trigger" :open="open" />
    </span>
    <transition
      enter-active-class="transition duration-150 ease-out"
      enter-from-class="opacity-0 -translate-y-1"
      leave-active-class="transition duration-100 ease-in"
      leave-to-class="opacity-0 -translate-y-1"
    >
      <div
        v-if="open"
        class="absolute z-30 min-w-44 rounded-md border border-border bg-popover p-1 shadow-lg shadow-black/20"
        :class="[align === 'right' ? 'right-0' : 'left-0', flipUp ? 'bottom-full mb-1.5' : 'top-full mt-1.5']"
        @click="close"
      >
        <slot />
      </div>
    </transition>
  </div>
</template>
