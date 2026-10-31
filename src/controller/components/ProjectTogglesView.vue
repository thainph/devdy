<script setup lang="ts">
/**
 * Per-project switches on the phone Controller (spec §2.B3): pick a project,
 * then turn MCP servers, skills and rules on or off for it. Switches only — the
 * Host never sends env / header values, tokens or accounts, and those stay
 * editable on the desktop alone.
 *
 * The built-in `devdy` MCP server is an app-wide setting today; while the Host
 * reports it `global`, its row says so, since flipping it changes every project.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, X } from 'lucide-vue-next'
import { AppSelect } from '@/components/ui'
import type { ProjectInfo, ProjectTogglesKind, ToggleItem, ToggleKind } from '../protocol'

const props = defineProps<{
  projects: ProjectInfo[]
  /** Switches of the selected project; null while loading. */
  toggles: ProjectTogglesKind | null
  /** The project shown; owned by the parent so it survives leaving the screen. */
  projectId: string | null
  error: string | null
}>()

const emit = defineEmits<{
  /** Show (and fetch) this project's switches. */
  select: [projectId: string]
  set: [kind: ToggleKind, itemId: string | null, enabled: boolean]
  'clear-error': []
}>()

const { t } = useI18n()

const selected = ref(props.projectId ?? '')

// Land on the first project when none is chosen yet, and fetch the chosen
// one's switches (also when coming back to this screen).
watch(
  () => [props.projects, props.projectId] as const,
  ([projects, pid]) => {
    if (pid) {
      selected.value = pid
    } else if (projects.length && !selected.value) {
      selected.value = projects[0].id
    }
  },
  { immediate: true },
)
watch(
  selected,
  (pid) => {
    if (pid) emit('select', pid)
  },
  { immediate: true },
)

const projectOptions = computed(() =>
  props.projects.map((p) => ({ value: p.id, label: p.name, description: p.path })),
)

/** Only show switches that belong to the selected project. */
const current = computed(() => (props.toggles?.project_id === selected.value ? props.toggles : null))

const sections = computed<{ kind: ToggleKind; label: string; items: ToggleItem[] }[]>(() => {
  const tg = current.value
  if (!tg) return []
  return [
    { kind: 'mcp', label: t('controller.projectToggles.mcp'), items: tg.mcp_servers },
    { kind: 'skill', label: t('controller.projectToggles.skills'), items: tg.skills },
    { kind: 'rule', label: t('controller.projectToggles.rules'), items: tg.rules },
  ]
})
</script>

<template>
  <div class="relative flex h-full min-h-0 flex-col bg-background text-foreground">
    <header class="shrink-0 flex items-center gap-2 px-3 py-2 border-b border-border">
      <h1 class="text-base font-semibold">{{ t('controller.projectToggles.title') }}</h1>
    </header>

    <div
      v-if="error"
      class="shrink-0 flex items-start gap-2 border-b border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive"
    >
      <span class="flex-1">{{ error }}</span>
      <button class="shrink-0 cursor-pointer" :title="t('controller.data.dismiss')" @click="emit('clear-error')">
        <X class="h-3.5 w-3.5" :stroke-width="2" />
      </button>
    </div>

    <main class="flex-1 min-h-0 overflow-auto px-3 py-3">
      <p v-if="!projects.length" class="py-10 text-center text-sm text-foreground/50">
        {{ t('controller.projectToggles.noProjects') }}
      </p>
      <template v-else>
        <AppSelect
          v-model="selected"
          size="md"
          class="w-full"
          :placeholder="t('controller.projectToggles.selectProject')"
          :options="projectOptions"
        />

        <p v-if="!current" class="flex items-center justify-center gap-2 py-10 text-sm text-foreground/50">
          <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" /> {{ t('controller.data.loading') }}
        </p>

        <template v-else>
          <section v-for="s in sections" :key="s.kind">
            <h2 class="mt-4 mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
              {{ s.label }}
            </h2>
            <div class="overflow-hidden rounded-xl border border-border bg-card">
              <!-- The built-in server heads the MCP section. -->
              <div
                v-if="s.kind === 'mcp'"
                class="flex items-center gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0"
              >
                <div class="min-w-0 flex-1">
                  <p class="truncate text-sm font-medium">{{ t('controller.projectToggles.builtin') }}</p>
                  <p class="truncate text-[11px] text-muted-foreground">
                    {{
                      current.builtin_devdy.global
                        ? t('controller.projectToggles.builtinGlobal')
                        : t('controller.projectToggles.builtinHint')
                    }}
                  </p>
                </div>
                <button
                  type="button"
                  role="switch"
                  :aria-checked="current.builtin_devdy.enabled"
                  :aria-label="
                    current.builtin_devdy.enabled
                      ? t('controller.projectToggles.disable', { name: t('controller.projectToggles.builtin') })
                      : t('controller.projectToggles.enable', { name: t('controller.projectToggles.builtin') })
                  "
                  class="relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
                  :class="current.builtin_devdy.enabled ? 'bg-emerald-500' : 'bg-muted'"
                  @click="emit('set', 'builtin_mcp', null, !current.builtin_devdy.enabled)"
                >
                  <span
                    class="inline-block h-5 w-5 rounded-full bg-white shadow transition-transform"
                    :class="current.builtin_devdy.enabled ? 'translate-x-[22px]' : 'translate-x-0.5'"
                  />
                </button>
              </div>

              <div
                v-for="item in s.items"
                :key="item.id"
                class="flex items-center gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0"
              >
                <p class="min-w-0 flex-1 truncate text-sm font-medium">{{ item.name }}</p>
                <button
                  type="button"
                  role="switch"
                  :aria-checked="item.enabled"
                  :aria-label="
                    item.enabled
                      ? t('controller.projectToggles.disable', { name: item.name })
                      : t('controller.projectToggles.enable', { name: item.name })
                  "
                  class="relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
                  :class="item.enabled ? 'bg-emerald-500' : 'bg-muted'"
                  @click="emit('set', s.kind, item.id, !item.enabled)"
                >
                  <span
                    class="inline-block h-5 w-5 rounded-full bg-white shadow transition-transform"
                    :class="item.enabled ? 'translate-x-[22px]' : 'translate-x-0.5'"
                  />
                </button>
              </div>

              <p
                v-if="!s.items.length && s.kind !== 'mcp'"
                class="px-3 py-2.5 text-xs text-muted-foreground"
              >
                {{ t('controller.projectToggles.none') }}
              </p>
            </div>
          </section>
        </template>
      </template>

      <p class="mt-5 mb-2 text-center text-xs text-muted-foreground">{{ t('controller.projectToggles.footer') }}</p>
    </main>
  </div>
</template>
