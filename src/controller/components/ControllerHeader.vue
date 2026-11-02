<script setup lang="ts">
/**
 * The top bar every Controller tab and editor shares, so the screens read as
 * one app: an optional leading control (Back), the screen title, the actions
 * on the right and, in the top-right corner, an optional badge (connection
 * state).
 *
 * With `scope` set, the title also names the project the screen follows
 * ("Sessions · devdy ▾") and the whole title is one wide tap target that asks
 * for the project picker (`pick-scope`).
 */
import { useI18n } from 'vue-i18n'
import { ChevronDown } from 'lucide-vue-next'

defineProps<{
  title: string
  /** The project label shown after the title; omit for no picker. */
  scope?: string
}>()

const emit = defineEmits<{ 'pick-scope': [] }>()

const { t } = useI18n()
</script>

<template>
  <header class="app-titlebar shrink-0 flex h-12 items-center gap-2 px-3 border-b border-border">
    <slot name="leading" />
    <button
      v-if="scope !== undefined"
      type="button"
      class="-mx-1.5 flex h-10 min-w-0 shrink cursor-pointer items-center gap-1 rounded-lg px-1.5 text-left active:bg-accent"
      :aria-label="t('controller.projectPicker.open', { name: scope })"
      @click="emit('pick-scope')"
    >
      <h1 class="shrink-0 text-base font-semibold">{{ title }}</h1>
      <span class="min-w-0 truncate text-base font-normal text-muted-foreground">· {{ scope }}</span>
      <ChevronDown class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
    </button>
    <h1 v-else class="min-w-0 shrink truncate text-base font-semibold">{{ title }}</h1>
    <div class="ml-auto flex min-w-0 shrink-0 items-center gap-1">
      <slot />
    </div>
    <slot name="badge" />
  </header>
</template>
