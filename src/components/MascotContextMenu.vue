<script setup lang="ts">
// Right-click context menu for the DY Cyber Fox (shared by the in-app floating
// mascot and the desktop-pet window). Rendered at the cursor, clamped to the
// viewport, and dismissed on outside-click / Escape / scroll / blur.
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import type { Component } from 'vue'

export interface MascotMenuItem {
  key: string
  label: string
  icon: Component
}

const props = defineProps<{
  open: boolean
  x: number
  y: number
  items: MascotMenuItem[]
}>()

const emit = defineEmits<{
  (e: 'select', key: string): void
  (e: 'close'): void
}>()

const MARGIN = 8
const menuRef = ref<HTMLElement | null>(null)
const pos = ref({ x: props.x, y: props.y })

// Place the menu at the cursor, nudging it back inside the viewport if it would
// overflow the right/bottom edges (important in the small desktop-pet window).
async function reposition() {
  pos.value = { x: props.x, y: props.y }
  await nextTick()
  const el = menuRef.value
  if (!el) return
  const { width, height } = el.getBoundingClientRect()
  let x = props.x
  let y = props.y
  if (x + width + MARGIN > window.innerWidth) x = window.innerWidth - width - MARGIN
  if (y + height + MARGIN > window.innerHeight) y = window.innerHeight - height - MARGIN
  pos.value = { x: Math.max(MARGIN, x), y: Math.max(MARGIN, y) }
}

function onGlobalPointerDown(e: PointerEvent) {
  if (menuRef.value && !menuRef.value.contains(e.target as Node)) emit('close')
}
function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    e.preventDefault()
    emit('close')
  }
}
function onDismiss() {
  emit('close')
}

function bindGlobal() {
  window.addEventListener('pointerdown', onGlobalPointerDown, true)
  window.addEventListener('keydown', onKeydown, true)
  window.addEventListener('scroll', onDismiss, true)
  window.addEventListener('blur', onDismiss)
  window.addEventListener('resize', onDismiss)
}
function unbindGlobal() {
  window.removeEventListener('pointerdown', onGlobalPointerDown, true)
  window.removeEventListener('keydown', onKeydown, true)
  window.removeEventListener('scroll', onDismiss, true)
  window.removeEventListener('blur', onDismiss)
  window.removeEventListener('resize', onDismiss)
}

watch(
  () => props.open,
  (open) => {
    if (open) {
      reposition()
      bindGlobal()
    } else {
      unbindGlobal()
    }
  },
)

// Re-clamp if the cursor coordinates change while already open.
watch(
  () => [props.x, props.y],
  () => {
    if (props.open) reposition()
  },
)

onBeforeUnmount(unbindGlobal)

const style = computed(() => ({ left: `${pos.value.x}px`, top: `${pos.value.y}px` }))

function pick(key: string) {
  emit('select', key)
  emit('close')
}
</script>

<template>
  <Teleport to="body">
    <Transition
      enter-active-class="transition duration-100 ease-out"
      enter-from-class="opacity-0 scale-95"
      enter-to-class="opacity-100 scale-100"
      leave-active-class="transition duration-75 ease-in"
      leave-from-class="opacity-100 scale-100"
      leave-to-class="opacity-0 scale-95"
    >
      <div
        v-if="open"
        ref="menuRef"
        class="mascot-context-menu"
        :style="style"
        role="menu"
        @contextmenu.prevent
      >
        <button
          v-for="item in items"
          :key="item.key"
          type="button"
          class="mascot-context-item"
          role="menuitem"
          @click="pick(item.key)"
        >
          <component :is="item.icon" class="h-4 w-4 text-primary shrink-0" :stroke-width="1.75" />
          <span class="truncate">{{ item.label }}</span>
        </button>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.mascot-context-menu {
  position: fixed;
  z-index: 9999;
  min-width: 168px;
  padding: 4px;
  border-radius: 10px;
  border: 1px solid hsl(var(--border) / 0.7);
  background: hsl(var(--popover));
  color: hsl(var(--popover-foreground, var(--foreground)));
  box-shadow: 0 12px 32px -10px rgb(0 0 0 / 0.55);
  transform-origin: top left;
}

.mascot-context-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 7px 10px;
  border-radius: 6px;
  font-size: 13px;
  line-height: 1.1;
  text-align: left;
  color: hsl(var(--foreground));
  cursor: pointer;
  transition: background-color 120ms ease;
}
.mascot-context-item:hover {
  background: hsl(var(--accent));
}
</style>
