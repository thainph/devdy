<script setup lang="ts">
/** The search row under a tab header (Sessions, Todo, Notes), with its own clear button. */
import { Search, X } from 'lucide-vue-next'

defineProps<{
  placeholder: string
  clearLabel: string
}>()

const query = defineModel<string>({ required: true })
</script>

<template>
  <div class="shrink-0 px-3 py-2 border-b border-border/40">
    <div class="relative">
      <Search
        class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-foreground/40"
        :stroke-width="2"
      />
      <input
        v-model="query"
        type="search"
        inputmode="search"
        autocomplete="off"
        :placeholder="placeholder"
        class="search-input ui-field w-full rounded-md border border-border bg-background py-1.5 pl-7 pr-7 text-sm outline-none placeholder:text-foreground/40 focus:border-foreground/30"
      />
      <button
        v-if="query.trim()"
        type="button"
        class="absolute right-1.5 top-1/2 inline-flex h-5 w-5 -translate-y-1/2 items-center justify-center rounded text-foreground/40 hover:text-foreground cursor-pointer"
        :title="clearLabel"
        @click="query = ''"
      >
        <X class="h-3.5 w-3.5" :stroke-width="2" />
      </button>
    </div>
  </div>
</template>

<style scoped>
/* `type="search"` gets a native clear button in WebKit/Blink, which would sit
   next to ours — two X's side by side. */
.search-input::-webkit-search-cancel-button,
.search-input::-webkit-search-decoration {
  -webkit-appearance: none;
  appearance: none;
}
</style>
