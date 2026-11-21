<script setup lang="ts">
// The search field of a list-detail sidebar, with the list's one filter /
// sort menu opened from a sliders button inside it:
//
//   ( 🔍 Search todos…            ×  ⚙︎• )
//
// - `v-model`: the query; × clears it (and Esc inside the field, when not empty).
// - `#filters`: the menu's content (ListMenuLabel + ListMenuCheck rows,
//   DropdownSeparator, a final "Reset to defaults" DropdownItem). The menu
//   stays open while options are toggled. Without the slot there is no button.
// - `filterActive`: the view differs from its defaults — an accent dot on the
//   button.
// - ⌘F / Ctrl+F focuses the field while its screen is the one in view
//   (composables/useFindShortcut); `shortcut: false` opts out. The placeholder
//   gets the hint appended.
// - `focus()` is exposed.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Search, SlidersHorizontal, X } from 'lucide-vue-next'
import { Button, DropdownMenu } from '@/components/ui'
import { cn } from '@/lib/utils'
import { controlHeight, fieldBase } from '@/components/ui/controlStyles'
import { useFindShortcut } from '@/composables/useFindShortcut'
import { IS_MAC } from '@/lib/appMenu'

const props = withDefaults(
  defineProps<{
    placeholder?: string
    /** Accent dot on the filter button: the view is not on its defaults. */
    filterActive?: boolean
    /** Tooltip / aria-label of the filter button. */
    filterLabel?: string
    /** ⌘F focuses the field. */
    shortcut?: boolean
  }>(),
  { placeholder: undefined, filterActive: false, filterLabel: undefined, shortcut: true },
)

const query = defineModel<string>({ required: true })

const { t } = useI18n()
const root = ref<HTMLElement | null>(null)
const input = ref<HTMLInputElement | null>(null)

const hint = IS_MAC ? '⌘F' : 'Ctrl+F'
const shownPlaceholder = computed(() => {
  const base = props.placeholder ?? t('listPane.search')
  return props.shortcut ? `${base}  (${hint})` : base
})
const label = computed(() => props.filterLabel ?? t('listPane.filterSort'))

function focus() {
  input.value?.focus()
  input.value?.select()
}

if (props.shortcut) useFindShortcut(root, focus)

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape' && query.value) {
    e.stopPropagation()
    query.value = ''
  }
}

defineExpose({ focus })
</script>

<template>
  <div
    ref="root"
    data-find-shortcut
    :class="cn(fieldBase, controlHeight.sm, 'flex items-center gap-1.5 pl-2.5 pr-1 focus-within:ring-1 focus-within:ring-ring')"
  >
    <Search class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
    <input
      ref="input"
      v-model="query"
      type="text"
      class="h-full min-w-0 flex-1 bg-transparent text-xs text-foreground outline-none placeholder:text-muted-foreground"
      :placeholder="shownPlaceholder"
      :aria-label="placeholder ?? t('listPane.search')"
      @keydown="onKeydown"
    />
    <Button
      v-if="query"
      variant="ghost"
      size="icon-xs"
      class="shrink-0 rounded-full"
      :title="t('listPane.clearSearch')"
      :aria-label="t('listPane.clearSearch')"
      @click="query = ''; input?.focus()"
    >
      <X class="h-3 w-3" :stroke-width="2" />
    </Button>
    <DropdownMenu v-if="$slots.filters" align="right" keep-open class="shrink-0">
      <template #trigger="{ open }">
        <Button
          variant="ghost"
          size="icon-xs"
          class="relative rounded-full"
          :class="[(open || filterActive) && 'text-foreground', open && 'bg-accent']"
          :title="label"
          :aria-label="label"
          aria-haspopup="menu"
          :aria-expanded="open"
        >
          <SlidersHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
          <span
            v-if="filterActive"
            class="absolute right-0.5 top-0.5 h-1.5 w-1.5 rounded-full bg-primary"
            aria-hidden="true"
          />
        </Button>
      </template>
      <div role="menu" :aria-label="label" class="max-h-[min(70vh,26rem)] min-w-52 overflow-y-auto">
        <slot name="filters" />
      </div>
    </DropdownMenu>
  </div>
</template>
