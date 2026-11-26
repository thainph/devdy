<script setup lang="ts">
// Search box for PageHeader actions (Projects, PR Inbox, Mentions…). A w-56
// field while the header is wide; below 40rem (pageHeader.ts) an icon button
// that opens the field over the whole header row; ✕ / Esc clear and close it,
// leaving it empty closes it, a typed search stays applied. The icon is tinted while a search is applied.
// The wide field takes focus when its screen comes into view and on ⌘F / Ctrl+F
// (composables/useFindShortcut).
import { nextTick, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Search, X } from 'lucide-vue-next'
import { useFindShortcut } from '@/composables/useFindShortcut'
import Button from './Button.vue'
import Input from './Input.vue'

const props = defineProps<{
  placeholder: string
  /** Title of the ✕ that clears the search; defaults to "Clear". */
  clearLabel?: string
}>()

const search = defineModel<string>({ required: true })

const { t } = useI18n()
const expanded = ref(false)
const overlayInput = ref<InstanceType<typeof Input> | null>(null)
const wide = ref<HTMLElement | null>(null)
const wideInput = ref<InstanceType<typeof Input> | null>(null)

async function expand() {
  expanded.value = true
  await nextTick()
  ;(overlayInput.value?.$el as HTMLInputElement | undefined)?.focus()
}

function collapse() {
  expanded.value = false
}

function onEsc() {
  search.value = ''
  collapse()
}

function onBlur() {
  if (!search.value) collapse()
}

const clearTitle = () => props.clearLabel || t('common.clear')

function focusWide() {
  const el = wideInput.value?.$el as HTMLInputElement | undefined
  el?.focus()
  el?.select()
}

useFindShortcut(wide, focusWide, { autoFocus: true })
</script>

<template>
  <!-- Wide: the field in the actions row. -->
  <div ref="wide" data-find-shortcut class="relative hidden @[40rem]/page-header:block">
    <Search class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" />
    <Input
      ref="wideInput"
      v-model="search"
      size="sm"
      class="w-56 pl-7 pr-7"
      :placeholder="placeholder"
      :aria-label="placeholder"
      @keydown.esc="search = ''"
    />
    <button
      v-if="search"
      type="button"
      class="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-muted-foreground hover:text-foreground"
      :title="clearTitle()"
      :aria-label="clearTitle()"
      @click="search = ''"
    >
      <X class="h-3.5 w-3.5" :stroke-width="2" />
    </button>
  </div>

  <!-- Narrow: an icon, opening the field over the header row. -->
  <Button
    variant="outline"
    size="icon"
    class="shrink-0 @[40rem]/page-header:hidden"
    :class="search && 'border-primary/50 text-primary'"
    :title="search ? `${placeholder}: ${search}` : placeholder"
    :aria-label="placeholder"
    :aria-expanded="expanded"
    @click="expand"
  >
    <Search class="h-3.5 w-3.5" :stroke-width="1.75" />
  </Button>
  <div
    v-if="expanded"
    class="absolute inset-0 z-10 flex items-center gap-2 bg-background px-3 @[40rem]/page-header:hidden"
  >
    <div class="relative min-w-0 flex-1">
      <Search class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" />
      <Input
        ref="overlayInput"
        v-model="search"
        size="sm"
        class="pl-7"
        :placeholder="placeholder"
        :aria-label="placeholder"
        @keydown.esc.stop="onEsc"
        @blur="onBlur"
      />
    </div>
    <Button
      variant="ghost"
      size="icon"
      class="shrink-0"
      :title="search ? clearTitle() : t('common.close')"
      :aria-label="search ? clearTitle() : t('common.close')"
      @click="onEsc"
    >
      <X class="h-3.5 w-3.5" :stroke-width="2" />
    </Button>
  </div>
</template>
