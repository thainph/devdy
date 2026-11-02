<script setup lang="ts">
/**
 * The Setting tab: a menu, each row opening its own screen. The first row
 * picks the project (the shared project picker); the rows under it act on
 * that project (their "on / total" counts come from its switches once
 * loaded); then this phone's look and notifications, and the connection
 * itself (Refresh re-fetches every tab, Disconnect).
 */
import { computed, type Component } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  Bell,
  BellOff,
  ChevronRight,
  FolderOpen,
  LogOut,
  Palette,
  Plug,
  RefreshCw,
  ScrollText,
  Server,
  Sparkles,
  UserRound,
} from 'lucide-vue-next'
import { FOLLOW_HOST } from '@/lib/controllerAppearance'
import { appearanceOverride } from '../appearance'
import type { ProjectInfo, ProjectTogglesKind } from '../protocol'
import type { SettingsPage, ToggleGroup } from '../settingsMenu'

const props = defineProps<{
  projects: ProjectInfo[]
  projectId: string | null
  /** Switches of the selected project; null while loading. */
  toggles: ProjectTogglesKind | null
  /** Whether push is on for this phone; null when the Host has no push. */
  pushOn: boolean | null
}>()

const emit = defineEmits<{
  open: [page: SettingsPage]
  'pick-project': []
  refresh: []
  disconnect: []
}>()

const { t } = useI18n()

const projectName = computed(() => props.projects.find((p) => p.id === props.projectId)?.name ?? '')

/** "on / total" per screen, once the selected project's switches are in. */
const counts = computed<Partial<Record<ToggleGroup, string>>>(() => {
  const tg = props.toggles
  if (!tg || tg.project_id !== props.projectId) return {}
  const c = (items: { enabled: boolean }[]) => `${items.filter((i) => i.enabled).length}/${items.length}`
  return {
    mcp: c([tg.builtin_devdy, ...tg.mcp_servers]),
    skill: c(tg.skills),
    rule: c(tg.rules),
    server: c(tg.servers),
    account: c([
      ...tg.claude_accounts,
      ...tg.github_accounts,
      ...tg.gitlab_accounts,
      ...tg.aws_accounts,
      ...tg.azure_accounts,
    ]),
  }
})

const projectRows = computed<{ id: ToggleGroup; icon: Component; label: string }[]>(() => [
  { id: 'mcp', icon: Plug, label: t('controller.projectToggles.mcp') },
  { id: 'skill', icon: Sparkles, label: t('controller.projectToggles.skills') },
  { id: 'rule', icon: ScrollText, label: t('controller.projectToggles.rules') },
  { id: 'server', icon: Server, label: t('controller.projectToggles.servers') },
  { id: 'account', icon: UserRound, label: t('controller.projectToggles.accounts') },
])

const themeValue = computed(() => {
  const mode = appearanceOverride.value.theme
  if (!mode || mode === FOLLOW_HOST) return t('controller.theme.followHost')
  const key = { light: 'themeLight', dark: 'themeDark', system: 'themeSystem' }[mode]
  return key ? t(`settings.general.${key}`) : mode
})

const heading = 'mt-4 mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
const row =
  'flex w-full cursor-pointer items-center gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 text-left text-sm font-medium'
</script>

<template>
  <main class="h-full min-h-0 overflow-auto px-3 py-3 text-foreground">
    <template v-if="projects.length">
      <h2 :class="heading" class="mt-0">{{ t('controller.settingsMenu.project') }}</h2>
      <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
        <button type="button" :class="row" @click="emit('pick-project')">
          <FolderOpen class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
          <span class="shrink-0">{{ t('controller.settingsMenu.projectRow') }}</span>
          <span class="min-w-0 flex-1 truncate text-right text-xs text-muted-foreground">{{ projectName }}</span>
          <ChevronRight class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
        </button>
      </div>
      <div class="ui-card mt-3 overflow-hidden rounded-xl border border-border bg-card">
        <button v-for="r in projectRows" :key="r.id" type="button" :class="row" @click="emit('open', r.id)">
          <component :is="r.icon" class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
          <span class="min-w-0 flex-1 truncate">{{ r.label }}</span>
          <span v-if="counts[r.id]" class="shrink-0 text-xs tabular-nums text-muted-foreground">{{ counts[r.id] }}</span>
          <ChevronRight class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
        </button>
      </div>
    </template>
    <p v-else class="py-6 text-center text-sm text-foreground/50">{{ t('controller.projectToggles.noProjects') }}</p>

    <h2 :class="heading">{{ t('controller.settingsMenu.device') }}</h2>
    <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
      <button type="button" :class="row" @click="emit('open', 'theme')">
        <Palette class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
        <span class="min-w-0 flex-1 truncate">{{ t('controller.theme.title') }}</span>
        <span class="shrink-0 text-xs text-muted-foreground">{{ themeValue }}</span>
        <ChevronRight class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
      </button>
      <button v-if="pushOn !== null" type="button" :class="row" @click="emit('open', 'push')">
        <Bell v-if="pushOn" class="h-4 w-4 shrink-0 text-success" :stroke-width="2" />
        <BellOff v-else class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
        <span class="min-w-0 flex-1 truncate">{{ t('controller.push.title') }}</span>
        <span class="shrink-0 text-xs text-muted-foreground">
          {{ pushOn ? t('controller.push.statusOn') : t('controller.push.statusOff') }}
        </span>
        <ChevronRight class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
      </button>
    </div>

    <h2 :class="heading">{{ t('controller.projectToggles.connection') }}</h2>
    <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
      <button type="button" :class="row" @click="emit('refresh')">
        <RefreshCw class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
        {{ t('controller.browser.refresh') }}
      </button>
      <button type="button" :class="row" class="text-destructive" @click="emit('disconnect')">
        <LogOut class="h-4 w-4 shrink-0" :stroke-width="2" />
        {{ t('controller.session.disconnect') }}
      </button>
    </div>
  </main>
</template>
