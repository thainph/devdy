<script setup lang="ts">
// Delivery Radar settings in one place: the thresholds that classify issues
// (edited live — the store persists them), the people followed, and the entry
// to the done rules.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { CheckCheck, ChevronRight } from 'lucide-vue-next'
import { Button, Input, Modal, ToggleSwitch } from '@/components/ui'
import { useDeliveryRadarStore } from '@/stores/deliveryRadar'

const props = defineProps<{
  open: boolean
  canEditDoneRules: boolean
  /** Assignees in scope (and anyone hidden) with their open issues; `unassigned` = open issues with nobody. */
  members: { people: { login: string, count: number }[], unassigned: number }
}>()
const emit = defineEmits<{ close: [], doneRules: [] }>()

const { t } = useI18n()
const store = useDeliveryRadarStore()

const memberQuery = ref('')
// Followed people first, hidden ones after; unassigned last.
const memberRows = computed(() => {
  const q = memberQuery.value.trim().toLowerCase()
  const rows: { login: string | null, label: string, count: number }[] = props.members.people
    .map((p) => ({ login: p.login, label: p.login, count: p.count }))
  rows.push({ login: null, label: t('radar.unassigned'), count: props.members.unassigned })
  return rows
    .filter((r) => !q || r.label.toLowerCase().includes(q))
    .sort((a, b) =>
      (a.login === null ? 1 : 0) - (b.login === null ? 1 : 0) ||
      Number(store.isHidden(a.login)) - Number(store.isHidden(b.login)) ||
      a.label.localeCompare(b.label))
})
const followedCount = computed(() => props.members.people.length + 1 - store.hidden.length)
</script>

<template>
  <Modal :open="open" :title="t('radar.settings.title')" size="sm" scroll-body @close="emit('close')">
    <div class="space-y-5 p-4">
      <section class="space-y-3">
        <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('radar.settings.thresholdsTitle') }}</h3>

        <div class="flex items-center gap-3">
          <div class="min-w-0 flex-1">
            <div class="text-[13px]">{{ t('radar.status.due-soon') }}</div>
            <div class="text-[11px] text-muted-foreground">{{ t('radar.settings.dueSoonHint') }}</div>
          </div>
          <Input v-model.number="store.thresholds.dueSoonDays" type="number" min="0" class="w-16" />
          <span class="w-10 text-[11px] text-muted-foreground">{{ t('radar.days') }}</span>
        </div>

        <div class="flex items-center gap-3">
          <div class="min-w-0 flex-1">
            <div class="text-[13px]">{{ t('radar.status.stale') }}</div>
            <div class="text-[11px] text-muted-foreground">{{ t('radar.settings.staleHint') }}</div>
          </div>
          <Input v-model.number="store.thresholds.staleDays" type="number" min="0" class="w-16" />
          <span class="w-10 text-[11px] text-muted-foreground">{{ t('radar.days') }}</span>
        </div>

        <div class="flex items-center gap-3">
          <div class="min-w-0 flex-1">
            <div class="text-[13px]">{{ t('radar.settings.milestoneLag') }}</div>
            <div class="text-[11px] text-muted-foreground">{{ t('radar.milestoneLagHint') }}</div>
          </div>
          <Input v-model.number="store.thresholds.milestoneLagPct" type="number" min="0" class="w-16" />
          <span class="w-10 text-[11px] text-muted-foreground">%</span>
        </div>
      </section>

      <section class="space-y-2">
        <h3 class="flex items-center text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
          {{ t('radar.settings.membersTitle') }}
          <span class="ml-auto font-normal normal-case tracking-normal">
            {{ t('radar.settings.membersFollowed', { n: followedCount, total: members.people.length + 1 }) }}
          </span>
        </h3>
        <p class="text-[11px] text-muted-foreground">{{ t('radar.settings.membersHint') }}</p>
        <Input v-model="memberQuery" size="sm" class="w-full" :placeholder="t('radar.settings.membersSearch')" />
        <div class="max-h-64 overflow-auto rounded-md border border-border/70">
          <div
            v-for="m in memberRows" :key="m.login ?? '\u0000'"
            class="flex items-center gap-2.5 border-b border-border/40 px-3 py-1.5 last:border-b-0"
            :class="{ 'bg-muted/40': store.isHidden(m.login) }"
          >
            <span
              class="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-muted text-[11px] font-semibold text-muted-foreground"
              :class="{ 'opacity-50': store.isHidden(m.login) }"
            >{{ m.login === null ? '?' : m.label[0]?.toUpperCase() }}</span>
            <span class="flex min-w-0 flex-1 flex-col" :class="{ 'opacity-60': store.isHidden(m.login) }">
              <span class="flex items-center gap-1.5 text-[13px]">
                <span class="truncate" :class="{ italic: m.login === null }">{{ m.label }}</span>
                <span
                  v-if="m.login !== null && store.roleOf(m.login) === 'tester'"
                  class="shrink-0 rounded border border-info/30 bg-info/15 px-1 text-[10px] font-medium text-info"
                >{{ t('radar.tester') }}</span>
              </span>
              <span class="truncate text-[11px] text-muted-foreground">
                {{ m.login === null ? t('radar.settings.unassignedHint') : '@' + m.login }} · {{ t('radar.settings.memberIssues', { n: m.count }, m.count) }}
              </span>
            </span>
            <span v-if="store.isHidden(m.login)" class="shrink-0 text-[11px] text-muted-foreground">{{ t('radar.settings.memberHidden') }}</span>
            <ToggleSwitch
              :checked="!store.isHidden(m.login)"
              :label="t('radar.settings.follow', { name: m.label })"
              @toggle="store.setHidden(m.login, !store.isHidden(m.login))"
            />
          </div>
          <p v-if="memberRows.length === 0" class="px-3 py-2 text-[11px] text-muted-foreground">{{ t('radar.settings.noMembers') }}</p>
        </div>
      </section>

      <section class="space-y-2">
        <h3 class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">{{ t('radar.doneStatuses.button') }}</h3>
        <button
          class="flex w-full items-center gap-3 rounded-md border border-border/70 px-3 py-2.5 text-left transition-colors hover:bg-accent disabled:opacity-50 disabled:pointer-events-none"
          :disabled="!canEditDoneRules"
          @click="emit('doneRules')"
        >
          <CheckCheck class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
          <span class="min-w-0 flex-1">
            <span class="block text-[13px]">{{ t('radar.doneStatuses.title') }}</span>
            <span class="block text-[11px] text-muted-foreground">{{ t('radar.doneStatuses.buttonHint') }}</span>
          </span>
          <ChevronRight class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
        </button>
      </section>
    </div>

    <template #footer>
      <Button variant="outline" @click="emit('close')">{{ t('common.close') }}</Button>
    </template>
  </Modal>
</template>
