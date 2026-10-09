<script setup lang="ts">
// "Insert saved prompt" picker for the chat composer — same look and keyboard
// contract as CapturePicker: a search box (title + body), ↑ ↓ move, Enter
// inserts. Each row shows the prompt's label plus a one-line body preview.
//
// The host inserts the emitted body at the caret (own line, focus, never sends).
import { computed, nextTick, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { BookMarked, Search } from 'lucide-vue-next'
import { DropdownMenu } from '@/components/ui'
import { promptLabel, type SavedPrompt } from '@/lib/savedPrompts'

const props = defineProps<{
  prompts: SavedPrompt[]
  disabled?: boolean
}>()

const emit = defineEmits<{
  insert: [prompt: SavedPrompt]
}>()

const { t } = useI18n()

const open = ref(false)
const query = ref('')
const active = ref(0)
const searchEl = ref<HTMLInputElement | null>(null)
const rowEls = ref<HTMLButtonElement[]>([])

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  if (!q) return props.prompts
  return props.prompts.filter((p) => `${p.title}\n${p.body}`.toLowerCase().includes(q))
})

/**
 * One-line body preview. When the prompt has no title its label already is the
 * first body line, so preview the next non-empty line instead.
 */
function preview(p: SavedPrompt): string {
  const lines = p.body.split('\n').map((l) => l.trim()).filter(Boolean)
  return (p.title.trim() ? lines[0] : lines[1]) ?? ''
}

function onOpenChange(value: boolean) {
  open.value = value
  if (!value) return
  query.value = ''
  active.value = 0
  nextTick(() => searchEl.value?.focus())
}

// Rows are clicked programmatically on Enter so the DropdownMenu's own
// click-to-close runs exactly as for a mouse pick.
function onSearchKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  const n = filtered.value.length
  if (e.key === 'ArrowDown' && n) {
    e.preventDefault()
    active.value = (active.value + 1) % n
  } else if (e.key === 'ArrowUp' && n) {
    e.preventDefault()
    active.value = (active.value - 1 + n) % n
  } else if (e.key === 'Enter' && n) {
    e.preventDefault()
    rowEls.value[active.value]?.click()
  }
}

watch(query, () => {
  active.value = 0
})
watch(active, (i) => {
  rowEls.value[i]?.scrollIntoView({ block: 'nearest' })
})
</script>

<template>
  <DropdownMenu align="left" class="shrink-0" @update:open="onOpenChange">
    <template #trigger>
      <button
        class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
        :disabled="disabled"
        :title="t('run.savedPrompts')"
        :aria-label="t('run.savedPrompts')"
      >
        <BookMarked class="h-4 w-4" :stroke-width="2" />
      </button>
    </template>

    <div class="w-80">
      <!-- Clicks stop here so they don't close the menu. -->
      <div class="relative mb-1" @click.stop>
        <Search
          class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
          :stroke-width="1.75"
        />
        <input
          ref="searchEl"
          v-model="query"
          type="text"
          class="h-8 w-full rounded border border-border bg-background pl-7 pr-2 text-xs text-foreground outline-none focus:ring-1 focus:ring-ring"
          :placeholder="t('run.savedPromptsSearch')"
          @keydown="onSearchKeydown"
        >
      </div>

      <div class="max-h-72 overflow-y-auto">
        <p
          v-if="filtered.length === 0"
          class="px-2.5 py-4 text-center text-[11px] text-muted-foreground"
          @click.stop
        >
          {{ t('item.noResults') }}
        </p>
        <button
          v-for="(p, i) in filtered"
          v-else
          :key="p.id"
          :ref="(el) => { if (el) rowEls[i] = el as HTMLButtonElement }"
          type="button"
          class="flex w-full cursor-pointer items-start gap-2 rounded px-2.5 py-1.5 text-left transition-colors"
          :class="active === i ? 'bg-accent' : 'hover:bg-accent'"
          :title="p.body"
          @mouseenter="active = i"
          @click="emit('insert', p)"
        >
          <BookMarked class="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
          <span class="block min-w-0 flex-1">
            <span class="block w-full truncate text-[13px] text-popover-foreground">{{ promptLabel(p) }}</span>
            <span
              v-if="preview(p)"
              class="block w-full truncate text-[11px] text-muted-foreground"
            >{{ preview(p) }}</span>
          </span>
        </button>
      </div>
    </div>
  </DropdownMenu>
</template>
