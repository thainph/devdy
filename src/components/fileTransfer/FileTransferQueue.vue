<script setup lang="ts">
// Transfer queue of the File Transfer window: overall progress, then one row
// per file with its own progress and cancel / retry.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ArrowLeft, ArrowRight, Check, RotateCcw, X } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { formatSize } from '@/lib/ipc/nativeCore'
import { itemPercent, queueProgress, type TransferItem } from '@/lib/fileTransferCore'

const props = defineProps<{ items: TransferItem[] }>()

const emit = defineEmits<{
  cancel: [id: number]
  retry: [id: number]
  cancelAll: []
  retryFailed: []
  clear: []
}>()

const { t } = useI18n()

const progress = computed(() => queueProgress(props.items))
const busy = computed(() => progress.value.active + progress.value.queued > 0)
const hasFinished = computed(() => props.items.some((i) => i.status !== 'queued' && i.status !== 'running'))

function statusText(it: TransferItem): string {
  switch (it.status) {
    case 'queued':
      return t('fileTransfer.queue.queued')
    case 'running':
      return `${itemPercent(it)}%`
    case 'done':
      return t('fileTransfer.queue.done')
    case 'canceled':
      return t('fileTransfer.queue.canceled')
    default:
      return t('fileTransfer.queue.failed')
  }
}

/** Errors raised by name in fileTransferCore (`file_changed`…) shown translated. */
function errorText(error: string | null): string {
  if (!error) return ''
  if (error === 'file_changed') return t('fileTransfer.errors.fileChanged')
  if (error.startsWith('integrity check failed')) return t('fileTransfer.errors.integrity')
  return error
}
</script>

<template>
  <section class="flex min-h-0 flex-col rounded-lg border border-border/70 bg-card">
    <div class="flex h-9 shrink-0 items-center gap-3 border-b border-border/60 px-3">
      <span class="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">{{ t('fileTransfer.queue.title') }}</span>
      <template v-if="items.length">
        <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
          <div class="h-full rounded-full bg-primary transition-[width]" :style="{ width: `${progress.percent}%` }" />
        </div>
        <span class="shrink-0 text-[11px] tabular-nums text-muted-foreground">
          {{
            t('fileTransfer.queue.summary', {
              done: formatSize(progress.done),
              total: formatSize(progress.total),
              percent: progress.percent,
            })
          }}
          <span v-if="progress.failed" class="text-destructive">· {{ t('fileTransfer.queue.failedCount', { count: progress.failed }) }}</span>
        </span>
      </template>
      <span v-else class="flex-1" />
      <div class="flex shrink-0 items-center gap-1">
        <Button v-if="progress.failed" variant="ghost" size="xs" @click="emit('retryFailed')">
          <RotateCcw class="h-3 w-3" /> {{ t('fileTransfer.queue.retryFailed') }}
        </Button>
        <Button v-if="busy" variant="ghost" size="xs" @click="emit('cancelAll')">
          <X class="h-3 w-3" /> {{ t('fileTransfer.queue.cancelAll') }}
        </Button>
        <Button v-if="hasFinished" variant="ghost" size="xs" @click="emit('clear')">
          {{ t('fileTransfer.queue.clear') }}
        </Button>
      </div>
    </div>
    <div class="min-h-0 flex-1 overflow-auto px-1.5 py-1">
      <p v-if="items.length === 0" class="px-2 py-4 text-center text-xs text-muted-foreground">{{ t('fileTransfer.queue.empty') }}</p>
      <div v-for="it in items" :key="it.id" class="flex items-center gap-2 rounded-md px-2 py-1 text-xs hover:bg-accent/50">
        <ArrowRight v-if="it.direction === 'up'" class="h-3.5 w-3.5 shrink-0 text-primary" :title="t('fileTransfer.queue.up')" />
        <ArrowLeft v-else class="h-3.5 w-3.5 shrink-0 text-primary" :title="t('fileTransfer.queue.down')" />
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <span class="truncate" :title="it.dest ?? it.src">{{ it.label }}</span>
            <span class="shrink-0 tabular-nums text-muted-foreground">{{ formatSize(it.size) }}</span>
          </div>
          <div v-if="it.status === 'running'" class="mt-0.5 h-1 overflow-hidden rounded-full bg-muted">
            <div class="h-full rounded-full bg-primary transition-[width]" :style="{ width: `${itemPercent(it)}%` }" />
          </div>
          <p v-if="it.status === 'failed'" class="truncate text-[11px] text-destructive" :title="errorText(it.error)">
            {{ errorText(it.error) }}
          </p>
        </div>
        <span
          class="w-16 shrink-0 text-right tabular-nums"
          :class="it.status === 'failed' ? 'text-destructive' : it.status === 'done' ? 'text-emerald-500' : 'text-muted-foreground'"
        >
          <Check v-if="it.status === 'done'" class="inline h-3.5 w-3.5" />
          {{ statusText(it) }}
        </span>
        <Button
          v-if="it.status === 'queued' || it.status === 'running'"
          variant="ghost"
          size="icon-sm"
          :title="t('common.cancel')"
          @click="emit('cancel', it.id)"
        >
          <X class="h-3.5 w-3.5" />
        </Button>
        <Button
          v-else-if="it.status === 'failed' || it.status === 'canceled'"
          variant="ghost"
          size="icon-sm"
          :title="t('fileTransfer.queue.retry')"
          @click="emit('retry', it.id)"
        >
          <RotateCcw class="h-3.5 w-3.5" />
        </Button>
        <span v-else class="w-7 shrink-0" />
      </div>
    </div>
  </section>
</template>
