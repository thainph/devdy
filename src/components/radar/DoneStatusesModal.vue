<script setup lang="ts">
// Who is done when, on the Delivery Radar: which assignees are testers, and the
// stage of every board status (dev → QA → done), set once for all projects —
// statuses are merged by name across the projects' boards. A developer is done
// once an issue reaches QA, a tester only at done. Statuses nobody picked a
// stage for use `defaultStage`, so usually only a few rows need touching.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Modal, SegmentedControl } from '@/components/ui'
import PrAvatar from '@/components/prInbox/PrAvatar.vue'
import { RADAR_STAGES, defaultStage, statusDotClass, type RadarStage } from '@/lib/deliveryRadar'
import { useDeliveryRadarStore, type StatusOptionInfo } from '@/stores/deliveryRadar'

const props = defineProps<{ open: boolean, projects: { id: string, name: string }[] }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const store = useDeliveryRadarStore()

const options = ref<StatusOptionInfo[]>([])
/** Stage picked per lowercased status name (only the ones differing from the default are saved). */
const picked = ref<Record<string, RadarStage>>({})
/** Lowercased logins ticked as testers. */
const testers = ref<Set<string>>(new Set())

const projectName = computed(() => new Map(props.projects.map((p) => [p.id, p.name])))

const people = computed(() => {
  const seen = store.assigneesFor(props.projects.map((p) => p.id))
  // Testers saved earlier but absent from these projects stay listed so they can be unticked.
  for (const l of store.testers) if (!seen.some((a) => a.toLowerCase() === l)) seen.push(l)
  return seen.sort((a, b) => a.localeCompare(b))
})

const stageOptions = computed(() =>
  RADAR_STAGES.map((s) => ({ value: s, label: t(`radar.doneStatuses.stage.${s}`), title: t(`radar.doneStatuses.stageHint.${s}`) })),
)

watch(() => props.open, (open) => {
  if (!open) return
  const list = store.statusOptionsFor(props.projects.map((p) => p.id))
  const next: Record<string, RadarStage> = {}
  for (const o of list) {
    const key = o.name.toLowerCase()
    next[key] = store.stages[key] ?? defaultStage(o.name)
  }
  // Picks for statuses no longer on these boards stay listed so they can be reset.
  for (const [key, stage] of Object.entries(store.stages)) {
    if (stage && !(key in next)) {
      list.push({ name: key, color: null, count: 0, projectIds: [] })
      next[key] = stage
    }
  }
  options.value = list
  picked.value = next
  testers.value = new Set(store.testers)
}, { immediate: true })

function stageOf(o: StatusOptionInfo): RadarStage {
  return picked.value[o.name.toLowerCase()] ?? defaultStage(o.name)
}

function isCustom(o: StatusOptionInfo): boolean {
  return stageOf(o) !== defaultStage(o.name)
}

function setStage(o: StatusOptionInfo, stage: RadarStage) {
  picked.value = { ...picked.value, [o.name.toLowerCase()]: stage }
}

const anyCustom = computed(() => options.value.some(isCustom))

function resetAll() {
  picked.value = Object.fromEntries(options.value.map((o) => [o.name.toLowerCase(), defaultStage(o.name)]))
}

function projectsText(o: StatusOptionInfo): string {
  return o.projectIds.map((id) => projectName.value.get(id) ?? id).join(', ')
}

function toggleTester(login: string) {
  const next = new Set(testers.value)
  const key = login.toLowerCase()
  if (next.has(key)) next.delete(key)
  else next.add(key)
  testers.value = next
}

function save() {
  // Keep only real overrides, so later changes to the defaults still apply elsewhere.
  store.setStages(Object.fromEntries(options.value.filter(isCustom).map((o) => [o.name, stageOf(o)])))
  store.setTesters([...testers.value])
  emit('close')
}
</script>

<template>
  <Modal :open="open" :title="t('radar.doneStatuses.title')" size="lg" scroll-body @close="emit('close')">
    <div class="space-y-5 p-4">
      <!-- Testers -->
      <section class="space-y-2">
        <div>
          <h3 class="text-[13px] font-medium">{{ t('radar.doneStatuses.testersTitle') }}</h3>
          <p class="text-[11px] text-muted-foreground">{{ t('radar.doneStatuses.testersHint') }}</p>
        </div>
        <p
          v-if="!people.length"
          class="rounded-lg border border-dashed border-border/70 px-3 py-4 text-center text-[11px] text-muted-foreground"
        >
          {{ t('radar.doneStatuses.noPeople') }}
        </p>
        <div v-else class="flex flex-wrap gap-1.5">
          <button
            v-for="login in people"
            :key="login"
            type="button"
            class="flex items-center gap-1.5 rounded-full border py-0.5 pl-0.5 pr-2.5 text-[11px] transition-colors"
            :class="testers.has(login.toLowerCase())
              ? 'border-info/40 bg-info/15 text-info'
              : 'border-border/60 text-muted-foreground hover:text-foreground'"
            :aria-pressed="testers.has(login.toLowerCase())"
            @click="toggleTester(login)"
          >
            <PrAvatar :url="`https://github.com/${login}.png?size=40`" :name="login" :size="18" />
            {{ login }}
            <span v-if="testers.has(login.toLowerCase())" class="font-semibold">· {{ t('radar.tester') }}</span>
          </button>
        </div>
      </section>

      <!-- Status → stage -->
      <section class="space-y-2">
        <div class="flex items-start gap-3">
          <div class="min-w-0 flex-1">
            <h3 class="text-[13px] font-medium">{{ t('radar.doneStatuses.stagesTitle') }}</h3>
            <p class="text-[11px] text-muted-foreground">{{ t('radar.doneStatuses.hint') }}</p>
          </div>
          <Button v-if="anyCustom" size="xs" variant="ghost" class="shrink-0" @click="resetAll">
            {{ t('radar.doneStatuses.resetDefault') }}
          </Button>
        </div>

        <p
          v-if="!options.length"
          class="rounded-lg border border-dashed border-border/70 px-3 py-4 text-center text-[11px] text-muted-foreground"
        >
          {{ t('radar.doneStatuses.noOptions') }}
        </p>
        <div v-else class="overflow-hidden rounded-lg border border-border/70 divide-y divide-border/50">
          <div v-for="o in options" :key="o.name" class="flex items-center gap-3 px-3 py-2">
            <span class="h-2 w-2 shrink-0 rounded-full" :class="statusDotClass(o.color)" />
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5">
                <span class="truncate text-xs font-medium">{{ o.name }}</span>
                <span
                  v-if="isCustom(o)"
                  class="shrink-0 rounded-full border border-primary/30 bg-primary/10 px-1.5 text-[10px] text-primary"
                >{{ t('radar.doneStatuses.custom') }}</span>
              </div>
              <div class="truncate text-[10px] text-muted-foreground" :title="projectsText(o)">
                <span v-if="o.count" class="tabular-nums">{{ t('radar.doneStatuses.issueCount', { n: o.count }) }}</span>
                <span v-if="o.count && o.projectIds.length"> · </span>
                {{ projectsText(o) }}
              </div>
            </div>
            <SegmentedControl
              :model-value="stageOf(o)"
              :options="stageOptions"
              :stretch="false"
              @update:model-value="setStage(o, $event)"
            />
          </div>
        </div>
      </section>
    </div>
    <template #footer>
      <Button variant="ghost" @click="emit('close')">{{ t('common.cancel') }}</Button>
      <Button @click="save">{{ t('common.save') }}</Button>
    </template>
  </Modal>
</template>
