<script setup lang="ts">
// "Dock into main window" of a surface pop-out (PR Inbox, Calendar, Mentions,
// Radar…): the window hands itself to the main window as a tab and closes. It
// also answers the main window's "dock everything" (a screen was unplugged,
// lib/surfaceOpen `dockAllSurfaceWindows`), so mount it once per pop-out.
import { useI18n } from 'vue-i18n'
import { ArrowDownToLine } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { requestDock, type DockSurface } from '@/lib/surfaceDock'
import { useDockAll } from '@/composables/useDockAll'

const props = defineProps<{
  surface: DockSurface
  /** Where the window is, so the tab reopens there (selected item, project…). */
  params?: () => Record<string, string>
}>()

const { t } = useI18n()

function dock() {
  void requestDock(props.surface, props.params?.())
}

useDockAll(dock)
</script>

<template>
  <Button
    variant="outline"
    size="icon"
    class="shrink-0"
    :title="t('surfaces.dock')"
    :aria-label="t('surfaces.dock')"
    @click="dock"
  >
    <ArrowDownToLine class="h-3.5 w-3.5" :stroke-width="1.75" />
  </Button>
</template>
