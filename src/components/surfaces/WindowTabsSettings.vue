<script setup lang="ts">
// Settings → "Windows & tabs": where the surfaces (PR Inbox, Calendar, Slack
// threads, Todos, Notes, files) open — globally and per screen — and whether
// windows dock back as tabs when a screen is unplugged. Writes its keys itself
// (the rest of SettingsView auto-saves a local copy; these never go through it).
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AppWindow } from 'lucide-vue-next'
import { invoke } from '@/lib/tauri'
import { AppSelect, Card, SegmentedControl, ToggleSwitch } from '@/components/ui'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useToast } from '@/composables/useToast'
import { monitorCount } from '@/lib/surfacePlacement'
import { SURFACE_META } from '@/lib/surfaceMeta'
import {
  SURFACE_KINDS,
  parseOverrides,
  parseWindowMode,
  placementFor,
  type Placement,
  type SurfaceKind,
  type WindowMode,
} from '@/lib/surfaces'

const { t } = useI18n()
const { toast } = useToast()
const appSettings = useAppSettingsStore()

const screens = ref(1)
onMounted(async () => {
  await appSettings.ensureLoaded().catch(() => {})
  screens.value = await monitorCount(true)
})

const mode = computed(() => parseWindowMode(appSettings.settings?.window_mode))
const overrides = computed(() => parseOverrides(appSettings.settings?.window_mode_overrides))
const dockOnUnplug = computed(() => appSettings.settings?.window_dock_on_unplug !== 'false')
const permFloat = computed(() => appSettings.settings?.permission_float_when_background !== 'false')
const splitCompact = computed(() => appSettings.settings?.split_compact_sidebar !== 'false')

const modeOptions = computed(() =>
  (['auto', 'tab', 'window'] as WindowMode[]).map((v) => ({ value: v, label: t(`surfaces.settings.modes.${v}`) })),
)
const overrideOptions = computed(() => [
  { value: 'inherit', label: t('surfaces.settings.inherit') },
  { value: 'tab', label: t('surfaces.settings.asTab') },
  { value: 'window', label: t('surfaces.settings.asWindow') },
])

async function save(key: string, value: string) {
  try {
    await invoke('update_setting', { key, value })
    await appSettings.refresh()
  } catch (e) {
    toast.error(t('surfaces.settings.saveFailed', { error: String(e) }))
  }
}

function setOverride(kind: SurfaceKind, value: string) {
  const next: Partial<Record<SurfaceKind, Placement>> = { ...overrides.value }
  if (value === 'tab' || value === 'window') next[kind] = value
  else delete next[kind]
  void save('window_mode_overrides', JSON.stringify(next))
}

function nowLabel(kind: SurfaceKind): string {
  return placementFor(kind, mode.value, overrides.value, screens.value) === 'window'
    ? t('surfaces.settings.asWindow')
    : t('surfaces.settings.asTab')
}
</script>

<template>
  <Card body-class="p-4 space-y-4">
    <template #header>
      <AppWindow class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('surfaces.settings.title') }}</span>
      <span class="ml-auto text-[10px] text-muted-foreground">{{ t('surfaces.settings.monitors', { n: screens }, screens) }}</span>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('surfaces.settings.desc') }}</p>

    <div class="space-y-1.5">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('surfaces.settings.mode') }}</label>
      <SegmentedControl
        :model-value="mode"
        :options="modeOptions"
        @update:model-value="(v) => save('window_mode', v)"
      />
      <p class="text-[11px] text-muted-foreground leading-relaxed">{{ t('surfaces.settings.modeHint') }}</p>
    </div>

    <div class="space-y-1.5">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('surfaces.settings.perScreen') }}</label>
      <div class="divide-y divide-border/50 rounded-md border border-border/60">
        <div
          v-for="kind in SURFACE_KINDS"
          :key="kind"
          class="flex items-center gap-2.5 px-3 py-1.5"
        >
          <component :is="SURFACE_META[kind].icon" class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
          <span class="min-w-0 flex-1 truncate text-xs">{{ t(SURFACE_META[kind].labelKey) }}</span>
          <span class="hidden @[28rem]/panel:inline text-[11px] text-muted-foreground" :title="t('surfaces.settings.now')">{{ nowLabel(kind) }}</span>
          <AppSelect
            size="xs"
            class="w-40"
            :model-value="overrides[kind] ?? 'inherit'"
            :options="overrideOptions"
            :aria-label="`${t(SURFACE_META[kind].labelKey)} — ${t('surfaces.settings.opensAs')}`"
            @update:model-value="(v) => setOverride(kind, String(v))"
          />
        </div>
      </div>
    </div>

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
