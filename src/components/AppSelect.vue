<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, watch, nextTick } from 'vue'
import { ChevronDown, Check } from 'lucide-vue-next'
import { controlSize } from './ui/controlStyles'

interface Option {
  value: string
  label: string
}

const props = withDefaults(defineProps<{
  modelValue: string
  options: Option[]
  placeholder?: string
  // Mirrors Input's sizing (see controlStyles). Default 'sm' so a bare
  // <AppSelect> matches a bare <Input> on every screen.
  size?: 'sm' | 'md'
  variant?: 'default' | 'ghost'
  disabled?: boolean
}>(), {
  placeholder: 'Select…',
  size: 'sm',
  variant: 'default',
  disabled: false,
})

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const isOpen = ref(false)
const highlightedIndex = ref(-1)
const triggerRef = ref<HTMLButtonElement | null>(null)
const listRef = ref<HTMLUListElement | null>(null)
const containerRef = ref<HTMLDivElement | null>(null)

// The dropdown is teleported to <body> so no ancestor's `overflow-hidden`
// (e.g. the surrounding Card) can clip it. Position it with fixed coords
// derived from the trigger's bounding rect, kept in sync while open.
const dropdownStyle = ref<Record<string, string>>({})

// Keep in sync with the list's max-height below.
const MAX_DROPDOWN_HEIGHT = 240

function updatePosition() {
  const el = triggerRef.value
  if (!el) return
  const rect = el.getBoundingClientRect()
  const spaceBelow = window.innerHeight - rect.bottom
  const spaceAbove = rect.top
  // Flip the menu above the trigger when there isn't room below but there is
  // above — so a composer pinned to the bottom of the screen isn't clipped.
  const openUp = spaceBelow < MAX_DROPDOWN_HEIGHT && spaceAbove > spaceBelow
  const available = Math.max(120, (openUp ? spaceAbove : spaceBelow) - 8)
  dropdownStyle.value = openUp
    ? {
        position: 'fixed',
        bottom: `${window.innerHeight - rect.top + 4}px`,
        left: `${rect.left}px`,
        width: `${rect.width}px`,
        maxHeight: `${Math.min(MAX_DROPDOWN_HEIGHT, available)}px`,
      }
    : {
        position: 'fixed',
        top: `${rect.bottom + 4}px`,
        left: `${rect.left}px`,
        width: `${rect.width}px`,
        maxHeight: `${Math.min(MAX_DROPDOWN_HEIGHT, available)}px`,
      }
}

const selectedOption = computed(() =>
  props.options.find(o => o.value === props.modelValue) ?? null
)

const displayLabel = computed(() =>
  selectedOption.value?.label ?? props.placeholder
)

function open() {
  if (props.disabled) return
  isOpen.value = true
  highlightedIndex.value = props.options.findIndex(o => o.value === props.modelValue)
  if (highlightedIndex.value < 0) highlightedIndex.value = 0
  updatePosition()
  nextTick(() => scrollHighlightedIntoView())
}

function close() {
  isOpen.value = false
  highlightedIndex.value = -1
}

function toggle() {
  isOpen.value ? close() : open()
}

function select(value: string) {
  emit('update:modelValue', value)
  close()
  triggerRef.value?.focus()
}

function scrollHighlightedIntoView() {
  if (!listRef.value) return
  const el = listRef.value.children[highlightedIndex.value] as HTMLElement | undefined
  el?.scrollIntoView({ block: 'nearest' })
}

function onKeydown(e: KeyboardEvent) {
  if (!isOpen.value) {
    if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowDown') {
      e.preventDefault()
      open()
    }
    return
  }
  switch (e.key) {
    case 'ArrowDown':
      e.preventDefault()
      highlightedIndex.value = Math.min(highlightedIndex.value + 1, props.options.length - 1)
      scrollHighlightedIntoView()
      break
    case 'ArrowUp':
      e.preventDefault()
      highlightedIndex.value = Math.max(highlightedIndex.value - 1, 0)
      scrollHighlightedIntoView()
      break
    case 'Enter':
    case ' ':
      e.preventDefault()
      if (highlightedIndex.value >= 0) select(props.options[highlightedIndex.value].value)
      break
    case 'Escape':
    case 'Tab':
      close()
      break
  }
}

function onClickOutside(e: MouseEvent) {
  const target = e.target as Node
  const insideTrigger = containerRef.value?.contains(target)
  const insideList = listRef.value?.contains(target)
  if (!insideTrigger && !insideList) {
    close()
  }
}

function onReposition() {
  if (isOpen.value) updatePosition()
}

onMounted(() => {
  document.addEventListener('mousedown', onClickOutside)
  // `true` (capture) so we catch scrolls on any ancestor scroll container.
  window.addEventListener('scroll', onReposition, true)
  window.addEventListener('resize', onReposition)
})
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onClickOutside)
  window.removeEventListener('scroll', onReposition, true)
  window.removeEventListener('resize', onReposition)
})

watch(isOpen, (val) => {
  if (!val) highlightedIndex.value = -1
})
</script>

<template>
  <div ref="containerRef" class="relative min-w-0">
    <!-- Trigger -->
    <button
      ref="triggerRef"
      type="button"
      :disabled="disabled"
      :aria-haspopup="'listbox'"
      :aria-expanded="isOpen"
      :aria-label="displayLabel"
      class="flex w-full h-full items-center gap-2 rounded-md border transition-colors cursor-pointer select-none disabled:opacity-50 disabled:cursor-not-allowed"
      :class="[
        size === 'sm' ? controlSize.sm : controlSize.md,
        variant === 'ghost'
          ? 'border-transparent bg-transparent hover:bg-accent hover:text-accent-foreground'
          : 'border-border bg-background hover:bg-accent hover:text-accent-foreground',
        isOpen && variant !== 'ghost' && 'border-ring ring-1 ring-ring',
        isOpen && variant === 'ghost' && 'bg-accent text-accent-foreground',
      ]"
      @click="toggle"
      @keydown="onKeydown"
    >
      <span class="flex flex-1 items-center gap-1.5 min-w-0">
        <slot name="leading" />
        <span
          class="truncate min-w-0"
          :class="selectedOption ? 'text-foreground' : 'text-muted-foreground'"
        >{{ displayLabel }}</span>
      </span>
      <ChevronDown
        class="shrink-0 text-muted-foreground transition-transform duration-200"
        :class="[
          isOpen && 'rotate-180',
          size === 'sm' ? 'h-3 w-3' : 'h-3.5 w-3.5',
        ]"
        :stroke-width="2"
      />
    </button>

    <!-- Dropdown — teleported to body so it isn't clipped by ancestor overflow. -->
    <Teleport to="body">
    <Transition
      enter-active-class="transition duration-150 ease-out"
      enter-from-class="opacity-0 translate-y-[-4px]"
      enter-to-class="opacity-100 translate-y-0"
      leave-active-class="transition duration-100 ease-in"
      leave-from-class="opacity-100 translate-y-0"
      leave-to-class="opacity-0 translate-y-[-4px]"
    >
      <ul
        v-if="isOpen"
        ref="listRef"
        role="listbox"
        class="z-50 min-w-32 overflow-auto rounded-md border border-border bg-popover py-1 shadow-md focus:outline-none"
        :style="dropdownStyle"
        tabindex="-1"
        @keydown="onKeydown"
      >
        <li
          v-for="(option, i) in options"
          :key="option.value"
          role="option"
          :aria-selected="option.value === modelValue"
          class="relative flex items-center gap-2 cursor-pointer select-none transition-colors"
          :class="[
            size === 'sm' ? controlSize.sm : controlSize.md,
            highlightedIndex === i
              ? 'bg-accent text-accent-foreground'
              : 'text-foreground',
          ]"
          @mouseenter="highlightedIndex = i"
          @mouseleave="highlightedIndex = -1"
          @mousedown.prevent="select(option.value)"
        >
          <Check
            class="shrink-0 text-primary"
            :class="[
              option.value === modelValue ? 'opacity-100' : 'opacity-0',
              size === 'sm' ? 'h-3 w-3' : 'h-3.5 w-3.5',
            ]"
            :stroke-width="2.5"
          />
          <span class="truncate">{{ option.label }}</span>
        </li>
      </ul>
    </Transition>
    </Teleport>
  </div>
</template>
