<script setup lang="ts">
// Per project, which GitHub Project board statuses mean "the developer is
// done" (e.g. Ready for QA): issues in them stop counting as late on the
// Delivery Radar. A project left on the default uses `isDefaultDoneStatus`.
// Statuses are listed in board order, like the board's columns.
import { ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Checkbox, Modal } from '@/components/ui'
import { isDefaultDoneStatus, statusDotClass } from '@/lib/deliveryRadar'
import { useDeliveryRadarStore, type StatusOptionInfo } from '@/stores/deliveryRadar'

const props = defineProps<{ open: boolean, projects: { id: string, name: string }[] }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const store = useDeliveryRadarStore()

interface Draft {
  options: StatusOptionInfo[]
  /** Lowercased statuses ticked as done. */
  done: Set<string>
  /** Still on the default rule (nothing saved for the project). */
  isDefault: boolean
}
const drafts = ref<Record<string, Draft>>({})

function defaultDone(options: StatusOptionInfo[]): Set<string> {
  return new Set(options.filter((o) => isDefaultDoneStatus(o.name)).map((o) => o.name.toLowerCase()))
}

watch(() => props.open, (open) => {
  if (!open) return
  const next: Record<string, Draft> = {}
  for (const p of props.projects) {
    const saved = store.doneStatuses[p.id]
    const options = store.statusOptionsFor(p.id)
    // Saved statuses no longer on the board stay listed so they can be unticked.
    for (const s of saved ?? []) {
      if (!options.some((o) => o.name.toLowerCase() === s.toLowerCase())) options.push({ name: s, color: null, count: 0 })
    }
    next[p.id] = {
      options,
      done: saved ? new Set(saved.map((s) => s.toLowerCase())) : defaultDone(options),
      isDefault: !saved,
    }
  }
  drafts.value = next
}, { immediate: true })

function isTicked(projectId: string, o: StatusOptionInfo): boolean {
  return drafts.value[projectId]?.done.has(o.name.toLowerCase()) ?? false
}

function toggle(projectId: string, o: StatusOptionInfo, on: boolean) {
  const d = drafts.value[projectId]
  const done = new Set(d.done)
  if (on) done.add(o.name.toLowerCase())
  else done.delete(o.name.toLowerCase())
  drafts.value = { ...drafts.value, [projectId]: { ...d, done, isDefault: false } }
}

function resetDefault(projectId: string) {
  const d = drafts.value[projectId]
  drafts.value = { ...drafts.value, [projectId]: { ...d, done: defaultDone(d.options), isDefault: true } }
}

function save() {
  for (const [id, d] of Object.entries(drafts.value)) {
    store.setDoneStatuses(id, d.isDefault ? null : d.options.filter((o) => d.done.has(o.name.toLowerCase())).map((o) => o.name))
  }
  emit('close')
}
</script>

<template>
  <Modal :open="open" :title="t('radar.doneStatuses.title')" size="md" scroll-body @close="emit('close')">
    <div class="space-y-5 p-4">
      <p class="text-xs text-muted-foreground">{{ t('radar.doneStatuses.hint') }}</p>

      <section v-for="p in projects" :key="p.id" class="space-y-2">
        <div class="flex h-7 items-center gap-2">
          <h3 class="truncate text-[13px] font-medium">{{ p.name }}</h3>
          <span
            v-if="drafts[p.id]?.isDefault"
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
          <Checkbox
            v-for="o in drafts[p.id].options"
            :key="o.name"
            :model-value="isTicked(p.id, o)"
            class="w-full items-center px-3 py-2 transition-colors hover:bg-accent/50"
            :class="isTicked(p.id, o) ? 'bg-success/5' : ''"
            @update:model-value="toggle(p.id, o, $event)"
          >
            <span class="flex items-center gap-2">
              <span class="h-2 w-2 shrink-0 rounded-full" :class="statusDotClass(o.color)" />
              <span class="flex-1 truncate text-xs">{{ o.name }}</span>
              <span v-if="o.count" class="text-[11px] tabular-nums text-muted-foreground">
                {{ t('radar.doneStatuses.issueCount', { n: o.count }) }}
              </span>
              <span
                class="w-14 rounded-full border px-1.5 py-0.5 text-center text-[10px] font-medium"
                :class="isTicked(p.id, o)
                  ? 'border-success/30 bg-success/15 text-success'
                  : 'border-transparent text-muted-foreground/60'"
              >
                {{ isTicked(p.id, o) ? t('radar.doneStatuses.doneTag') : t('radar.doneStatuses.activeTag') }}
              </span>
            </span>
          </Checkbox>
        </div>
      </section>
    </div>
    <template #footer>
      <Button variant="ghost" @click="emit('close')">{{ t('common.cancel') }}</Button>
      <Button @click="save">{{ t('common.save') }}</Button>
    </template>
  </Modal>
</template>
