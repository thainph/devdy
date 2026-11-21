<script setup lang="ts">
// One tab of the main window's strip (components/WorkspaceTabs): a project
// (live status dot + pending-permission count), a page or a surface. Also each
// half of the split pair's merged tab (`half`). Clicks and drags are wired by
// the strip on this button.
import { useI18n } from 'vue-i18n'
import { ArrowLeftRight, Columns2, ExternalLink, FolderOpen, PanelRightOpen, X } from 'lucide-vue-next'
import type { TabEntry } from '@/stores/workspaceTabs'
import { entryIcon } from '@/lib/tabMeta'

defineProps<{
  tab: TabEntry
  label: string
  /** Live status of a project tab. */
  running?: boolean
  pending?: number
  /** The look: the focused tab / shown but not focused / idle. */
  look: 'active' | 'visible' | 'idle'
  /** A half of the pair's merged tab. */
  half?: boolean
  /**
   * The split hover button: 'open' shows this tab on the right, 'close' ends
   * the split (a half of the shown pair); null hides it.
   */
  splitAction?: 'open' | 'close' | null
  /** A half of the shown pair: offer to swap the two halves. */
  swappable?: boolean
}>()

const emit = defineEmits<{
  close: []
  toggleSplit: []
  swap: []
  popOut: []
}>()

const { t } = useI18n()

function onClose(ev: MouseEvent) {
  ev.stopPropagation()
  emit('close')
}
function onSplit(ev: MouseEvent) {
  ev.stopPropagation()
  emit('toggleSplit')
}
function onSwap(ev: MouseEvent) {
  ev.stopPropagation()
  emit('swap')
}
function onPopOut(ev: MouseEvent) {
  ev.stopPropagation()
  emit('popOut')
}

</script>

<template>
  <button
    type="button"
    class="group relative flex shrink-0 items-center gap-2 pl-3 pr-2 rounded-md text-[13px] transition-colors select-none touch-none cursor-pointer"
    :class="half
      ? [
        'h-full min-w-0 flex-1',
        look === 'active'
          ? 'bg-background text-foreground shadow-sm'
          : look === 'visible'
            ? 'text-foreground opacity-70 hover:opacity-100'
            : 'text-muted-foreground hover:text-foreground hover:bg-accent/50',
      ]
      : [
        'h-7',
        look === 'active'
          ? 'bg-accent text-foreground'
          : look === 'visible'
            ? 'bg-accent/50 text-foreground'
            : 'text-muted-foreground hover:text-foreground hover:bg-accent/50',
      ]"
  >
    <template v-if="tab.type === 'project'">
      <!-- status dot: running / awaiting permission -->
      <span class="relative flex h-2 w-2 shrink-0">
        <span
          v-if="running"
          class="absolute inline-flex h-full w-full rounded-full bg-primary opacity-60 animate-ping"
        />
        <span
          class="relative inline-flex h-2 w-2 rounded-full"
          :class="(pending ?? 0) > 0 ? 'bg-warning' : running ? 'bg-primary' : 'bg-muted-foreground/40'"
        />
      </span>
      <FolderOpen class="h-[13px] w-[13px] shrink-0 opacity-70" :stroke-width="1.75" />
    </template>
    <component :is="entryIcon(tab)" v-else class="h-[13px] w-[13px] shrink-0 opacity-70" :stroke-width="1.75" />

    <span class="min-w-0 flex-1 truncate text-left">{{ label }}</span>

    <!-- pending-permission badge (summed across the project's runs) -->
    <span
      v-if="tab.type === 'project' && (pending ?? 0) > 0"
      class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-warning px-1 text-[10px] font-medium text-warning-foreground leading-none shrink-0"
    >{{ pending }}</span>

    <!-- split view: open on the right / swap halves / close the split; pop out -->
    <span
      v-if="swappable"
      class="hidden h-4 w-4 items-center justify-center rounded shrink-0 group-hover:flex hover:bg-foreground/10"
      role="button"
      :title="t('surfaces.tabs.swapSplit')"
      :aria-label="t('surfaces.tabs.swapSplit')"
      @pointerdown.stop
      @click="onSwap"
    >
      <ArrowLeftRight class="h-3 w-3" :stroke-width="2" />
    </span>
    <span
      v-if="splitAction"
      class="hidden h-4 w-4 items-center justify-center rounded shrink-0 group-hover:flex hover:bg-foreground/10"
      role="button"
      :title="splitAction === 'open' ? t('surfaces.tabs.openOnRight') : t('surfaces.tabs.unsplit')"
      :aria-label="splitAction === 'open' ? t('surfaces.tabs.openOnRight') : t('surfaces.tabs.unsplit')"
      @pointerdown.stop
      @click="onSplit"
    >
      <component :is="splitAction === 'open' ? PanelRightOpen : Columns2" class="h-3 w-3" :stroke-width="2" />
    </span>
    <span
      v-if="tab.type === 'surface'"
      class="hidden h-4 w-4 items-center justify-center rounded shrink-0 group-hover:flex hover:bg-foreground/10"
      role="button"
      :title="t('surfaces.tabs.popOut')"
      :aria-label="t('surfaces.tabs.popOut')"
      @pointerdown.stop
      @click="onPopOut"
    >
      <ExternalLink class="h-3 w-3" :stroke-width="2" />
    </span>

    <!-- close -->
    <span
      class="flex h-4 w-4 items-center justify-center rounded shrink-0 opacity-0 group-hover:opacity-100 hover:bg-foreground/10 transition-opacity"
      :class="{ 'opacity-60': look === 'active' }"
      role="button"
      :aria-label="t('misc.tabs.closeTab')"
      @pointerdown.stop
      @click="onClose"
    >
      <X class="h-3 w-3" :stroke-width="2" />
    </span>
  </button>
</template>
