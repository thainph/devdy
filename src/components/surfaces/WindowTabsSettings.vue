<script setup lang="ts">
// Settings → "Windows & tabs": surfaces always open as tabs; this holds whether
// popped-out windows dock back as tabs when a screen is unplugged, approval
// pop-ups and the split-view sidebar. Writes its keys itself
// (the rest of SettingsView auto-saves a local copy; these never go through it).
import { computed, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { AppWindow } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { Card, ToggleSwitch } from '@/components/ui'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useToast } from '@/composables/useToast'

const { t } = useI18n()
const { toast } = useToast()
const appSettings = useAppSettingsStore()

onMounted(() => {
  appSettings.ensureLoaded().catch(() => {})
})

const dockOnUnplug = computed(() => appSettings.settings?.window_dock_on_unplug !== 'false')
const permFloat = computed(() => appSettings.settings?.permission_float_when_background !== 'false')
const splitCompact = computed(() => appSettings.settings?.split_compact_sidebar !== 'false')

async function save(key: string, value: string) {
  try {
    await invoke('update_setting', { key, value })
    await appSettings.refresh()
  } catch (e) {
    toast.error(t('surfaces.settings.saveFailed', { error: String(e) }))
  }
}
</script>

<template>
  <Card body-class="p-4 space-y-4">
    <template #header>
      <AppWindow class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('surfaces.settings.title') }}</span>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('surfaces.settings.desc') }}</p>

    <div class="flex items-start justify-between gap-3 border-t border-border/60 pt-3">
      <div class="min-w-0">
        <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('surfaces.settings.dockOnUnplug') }}</div>
        <p class="text-[11px] text-muted-foreground leading-relaxed mt-0.5">{{ t('surfaces.settings.dockOnUnplugHint') }}</p>
      </div>
      <ToggleSwitch
        size="md"
        :checked="dockOnUnplug"
        :label="t('surfaces.settings.dockOnUnplug')"
        @toggle="save('window_dock_on_unplug', dockOnUnplug ? 'false' : 'true')"
      />
    </div>

    <div class="flex items-start justify-between gap-3 border-t border-border/60 pt-3">
      <div class="min-w-0">
        <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('surfaces.settings.permFloat') }}</div>
        <p class="text-[11px] text-muted-foreground leading-relaxed mt-0.5">{{ t('surfaces.settings.permFloatHint') }}</p>
      </div>
      <ToggleSwitch
        size="md"
        :checked="permFloat"
        :label="t('surfaces.settings.permFloat')"
        @toggle="save('permission_float_when_background', permFloat ? 'false' : 'true')"
      />
    </div>

    <div class="flex items-start justify-between gap-3 border-t border-border/60 pt-3">
      <div class="min-w-0">
        <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('surfaces.settings.splitCompactSidebar') }}</div>
        <p class="text-[11px] text-muted-foreground leading-relaxed mt-0.5">{{ t('surfaces.settings.splitCompactSidebarHint') }}</p>
      </div>
      <ToggleSwitch
        size="md"
        :checked="splitCompact"
        :label="t('surfaces.settings.splitCompactSidebar')"
        @toggle="save('split_compact_sidebar', splitCompact ? 'false' : 'true')"
      />
    </div>
  </Card>
</template>
