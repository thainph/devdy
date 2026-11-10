<script setup lang="ts">
// Who is done when, on the Delivery Radar. Per project, which GitHub Project
// board statuses mean "the developer is done" (e.g. Ready for QA) and which
// mean "the tester is done" (e.g. Ready for release); and which assignees are
// testers. An issue drops off an assignee's list once it reaches a status done
// for their role. A project left on the default uses `isDefaultDoneStatus`.
// Statuses are listed in board order, like the board's columns.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Checkbox, Modal } from '@/components/ui'
import { isDefaultDoneStatus, statusDotClass, type RadarRole } from '@/lib/deliveryRadar'
import { useDeliveryRadarStore, type StatusOptionInfo } from '@/stores/deliveryRadar'

const props = defineProps<{ open: boolean, projects: { id: string, name: string }[] }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const store = useDeliveryRadarStore()

const ROLES: RadarRole[] = ['dev', 'tester']

interface RoleDraft {
  /** Lowercased statuses ticked as done. */
  done: Set<string>
  /** Still on the default rule (nothing saved for the project). */
  isDefault: boolean
}
interface Draft {
  options: StatusOptionInfo[]
  roles: Record<RadarRole, RoleDraft>
}
const drafts = ref<Record<string, Draft>>({})
/** Lowercased logins ticked as testers. */
const testers = ref<Set<string>>(new Set())

const people = computed(() => {
  const seen = store.assigneesFor(props.projects.map((p) => p.id))
  // Testers saved earlier but absent from these projects stay listed so they can be unticked.
  for (const l of store.testers) if (!seen.some((a) => a.toLowerCase() === l)) seen.push(l)
  return seen.sort((a, b) => a.localeCompare(b))
})

function defaultDone(options: StatusOptionInfo[], role: RadarRole): Set<string> {
  return new Set(options.filter((o) => isDefaultDoneStatus(o.name, role)).map((o) => o.name.toLowerCase()))
}

function savedFor(projectId: string, role: RadarRole): string[] | undefined {
  return (role === 'tester' ? store.qaDoneStatuses : store.doneStatuses)[projectId]
}

watch(() => props.open, (open) => {
  if (!open) return
  const next: Record<string, Draft> = {}
  for (const p of props.projects) {
    const options = store.statusOptionsFor(p.id)
    // Saved statuses no longer on the board stay listed so they can be unticked.
    for (const role of ROLES) {
      for (const s of savedFor(p.id, role) ?? []) {
        if (!options.some((o) => o.name.toLowerCase() === s.toLowerCase())) options.push({ name: s, color: null, count: 0 })
      }
    }
    const roleDraft = (role: RadarRole): RoleDraft => {
      const saved = savedFor(p.id, role)
      return {
        done: saved ? new Set(saved.map((s) => s.toLowerCase())) : defaultDone(options, role),
        isDefault: !saved,
      }
    }
    next[p.id] = { options, roles: { dev: roleDraft('dev'), tester: roleDraft('tester') } }
  }
  drafts.value = next
  testers.value = new Set(store.testers)
}, { immediate: true })

function isTicked(projectId: string, role: RadarRole, o: StatusOptionInfo): boolean {
  return drafts.value[projectId]?.roles[role].done.has(o.name.toLowerCase()) ?? false
}

function isDefault(projectId: string): boolean {
  const d = drafts.value[projectId]
  return !d || (d.roles.dev.isDefault && d.roles.tester.isDefault)
}

function toggle(projectId: string, role: RadarRole, o: StatusOptionInfo, on: boolean) {
  const d = drafts.value[projectId]
  const done = new Set(d.roles[role].done)
  if (on) done.add(o.name.toLowerCase())
  else done.delete(o.name.toLowerCase())
  drafts.value = { ...drafts.value, [projectId]: { ...d, roles: { ...d.roles, [role]: { done, isDefault: false } } } }
}

function resetDefault(projectId: string) {
  const d = drafts.value[projectId]
  drafts.value = {
    ...drafts.value,
    [projectId]: {
      ...d,
      roles: {
        dev: { done: defaultDone(d.options, 'dev'), isDefault: true },
        tester: { done: defaultDone(d.options, 'tester'), isDefault: true },
      },
    },
  }
}

function toggleTester(login: string, on: boolean) {
  const next = new Set(testers.value)
  if (on) next.add(login.toLowerCase())
  else next.delete(login.toLowerCase())
  testers.value = next
}

function save() {
  for (const [id, d] of Object.entries(drafts.value)) {
    for (const role of ROLES) {
      const r = d.roles[role]
      store.setDoneStatuses(id, r.isDefault ? null : d.options.filter((o) => r.done.has(o.name.toLowerCase())).map((o) => o.name), role)
    }
  }
  store.setTesters([...testers.value])
  emit('close')
}
</script>

<template>
  <Modal :open="open" :title="t('radar.doneStatuses.title')" size="md" scroll-body @close="emit('close')">
    <div class="space-y-5 p-4">
      <p class="text-xs text-muted-foreground">{{ t('radar.doneStatuses.hint') }}</p>

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
        <div v-else class="flex flex-wrap gap-x-4 gap-y-2 rounded-lg border border-border/70 px-3 py-2.5">
          <Checkbox
            v-for="login in people"
            :key="login"
            :model-value="testers.has(login.toLowerCase())"
            @update:model-value="toggleTester(login, $event)"
          >
            <span class="text-xs">{{ login }}</span>
          </Checkbox>
        </div>
      </section>

      <section v-for="p in projects" :key="p.id" class="space-y-2">
        <div class="flex h-7 items-center gap-2">
          <h3 class="truncate text-[13px] font-medium">{{ p.name }}</h3>
          <span
            v-if="isDefault(p.id)"
            class="rounded-full border border-border/70 bg-muted px-2 py-0.5 text-[10px] text-muted-foreground"
          >
            {{ t('radar.doneStatuses.usingDefault') }}
          </span>
          <Button
            v-else
            size="xs"
            variant="ghost"
            class="ml-auto"
            @click="resetDefault(p.id)"
          >
            {{ t('radar.doneStatuses.resetDefault') }}
          </Button>
        </div>

        <p
          v-if="!drafts[p.id]?.options.length"
          class="rounded-lg border border-dashed border-border/70 px-3 py-4 text-center text-[11px] text-muted-foreground"
        >
          {{ t('radar.doneStatuses.noOptions') }}
        </p>
        <div v-else class="overflow-hidden rounded-lg border border-border/70 divide-y divide-border/50">
          <div class="flex items-center gap-2 bg-muted/30 px-3 py-1.5 text-[10px] font-medium text-muted-foreground">
            <span class="flex-1">{{ t('radar.doneStatuses.colStatus') }}</span>
            <span v-for="role in ROLES" :key="role" class="w-16 text-center">{{ t(`radar.doneStatuses.col.${role}`) }}</span>
          </div>
          <div v-for="o in drafts[p.id].options" :key="o.name" class="flex items-center gap-2 px-3 py-2">
            <span class="h-2 w-2 shrink-0 rounded-full" :class="statusDotClass(o.color)" />
            <span class="flex-1 truncate text-xs">{{ o.name }}</span>
            <span v-if="o.count" class="text-[11px] tabular-nums text-muted-foreground">
              {{ t('radar.doneStatuses.issueCount', { n: o.count }) }}
            </span>
            <span v-for="role in ROLES" :key="role" class="flex w-16 justify-center">
              <Checkbox
                :model-value="isTicked(p.id, role, o)"
                :label="`${o.name} — ${t(`radar.doneStatuses.col.${role}`)}`"
                @update:model-value="toggle(p.id, role, o, $event)"
              />
            </span>
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
