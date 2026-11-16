<script setup lang="ts">
// Delivery Radar settings in one place: the thresholds that classify issues
// (edited live — the store persists them) and the entry to the done rules.
import { useI18n } from 'vue-i18n'
import { CheckCheck, ChevronRight } from 'lucide-vue-next'
import { Button, Input, Modal } from '@/components/ui'
import { useDeliveryRadarStore } from '@/stores/deliveryRadar'

defineProps<{ open: boolean, canEditDoneRules: boolean }>()
const emit = defineEmits<{ close: [], doneRules: [] }>()

const { t } = useI18n()
const store = useDeliveryRadarStore()
</script>

<template>
  <Modal :open="open" :title="t('radar.settings.title')" size="sm" @close="emit('close')">
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
