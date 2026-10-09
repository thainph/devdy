<script setup lang="ts">
// The slim titlebar shared by the detail pop-outs — the note / todo window
// (views/ItemWindow.vue) and the capture window (views/CaptureWindow.vue):
// a kind icon, which item this is, an "unsaved" marker, then the optional
// open-the-source link (Slack / original page), pin and reload.
// It only says what the window alone can show; every ACTION lives in the
// host's action bar.
import type { Component } from 'vue'
import { useI18n } from 'vue-i18n'
import { ExternalLink, RotateCw } from 'lucide-vue-next'
import WindowPinButton from '@/components/WindowPinButton.vue'

defineProps<{
  icon: Component
  /** The label, also its tooltip. The `label` slot overrides how it renders. */
  title: string
  dirty?: boolean
  pinned: boolean
  reloading?: boolean
  /** Tooltip / aria-label of the open-the-source button; omitted = no button. */
  externalLabel?: string
}>()

defineEmits<{ togglePin: []; reload: []; openExternal: [] }>()

const { t } = useI18n()
</script>

<template>
  <div class="flex items-center gap-2 px-3 h-9 border-b border-border/60 shrink-0">
    <component :is="icon" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
    <slot name="label">
      <span class="truncate text-xs text-muted-foreground" :title="title">{{ title }}</span>
    </slot>
    <span v-if="dirty" class="shrink-0 text-[11px] text-muted-foreground/70">
      · {{ t('item.unsaved') }}
    </span>

    <span class="ml-auto" />
    <button
      v-if="externalLabel"
      type="button"
      class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
      :title="externalLabel"
      :aria-label="externalLabel"
      @click="$emit('openExternal')"
    >
      <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
    </button>
    <WindowPinButton :pinned="pinned" @toggle="$emit('togglePin')" />
    <!-- Reload the latest content from the DB (another window / the AI may have changed it). -->
    <button
      type="button"
      class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0 disabled:opacity-40 disabled:cursor-default"
      :title="t('item.reload')"
      :aria-label="t('item.reload')"
      :disabled="reloading"
      @click="$emit('reload')"
    >
      <RotateCw class="h-3.5 w-3.5" :class="{ 'animate-spin': reloading }" :stroke-width="1.75" />
    </button>
  </div>
</template>
