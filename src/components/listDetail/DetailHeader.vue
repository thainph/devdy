<script setup lang="ts">
// Header of a list-detail screen's right pane — the Notes window's ItemHeader
// look: a bold title (click to rename inline when `renamable`), a muted meta
// line (slot), and the actions on the right (slot).
import { ref } from 'vue'
import InlineRenameInput from '@/components/InlineRenameInput.vue'

const props = withDefaults(
  defineProps<{
    title: string
    /** Shown when `title` is empty. */
    placeholder?: string
    renamable?: boolean
    renameHint?: string
  }>(),
  { placeholder: '', renamable: false, renameHint: '' },
)

const emit = defineEmits<{
  /** Trimmed, non-empty and different from `title`. */
  rename: [value: string]
}>()

defineSlots<{
  /** Before the title (e.g. a status dot or an icon). */
  leading?: () => unknown
  meta?: () => unknown
  actions?: () => unknown
}>()

const renaming = ref(false)

function commit(value: string) {
  renaming.value = false
  if (value && value !== props.title) emit('rename', value)
}
</script>

<template>
  <div class="flex shrink-0 items-center gap-3 border-b border-border/60 px-5 py-2.5">
    <div class="min-w-[6rem] flex-1">
      <div class="flex min-w-0 items-center gap-1.5">
        <slot name="leading" />
        <InlineRenameInput
          v-if="renaming"
          class="w-full text-[15px] font-bold leading-snug"
          :value="title"
          :placeholder="placeholder"
          @commit="commit"
          @cancel="renaming = false"
        />
        <h1
          v-else
          class="min-w-0 truncate rounded px-0.5 -mx-0.5 text-[15px] font-bold leading-snug"
          :class="[
            renamable && 'cursor-text transition-colors hover:bg-accent/50',
            !title && 'text-muted-foreground',
          ]"
          :title="renamable && renameHint ? `${title}\n${renameHint}` : title"
          @click="renamable && (renaming = true)"
        >
          {{ title || placeholder }}
        </h1>
      </div>
      <p v-if="$slots.meta" class="mt-0.5 truncate text-xs text-muted-foreground">
        <slot name="meta" />
      </p>
    </div>
    <div class="flex min-w-0 shrink-0 items-center gap-1.5">
      <slot name="actions" />
    </div>
  </div>
</template>
