<script setup lang="ts">
// Settings → Devices, "Recent activity": the Remote Control audit log, folded by default
// (one summary line), with readable action / reason labels when opened.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronRight, Loader2, RefreshCw, ShieldCheck, Trash2 } from 'lucide-vue-next'
import { Badge, Button, Card } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useRemoteControlStore, type AuditEntry } from '@/stores/remoteControl'
import { AUDIT_ACTION_LABELS, AUDIT_REASON_LABELS, humanizeAction } from '@/lib/remoteDevice'

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const store = useRemoteControlStore()

const open = ref(false)
const clearing = ref(false)
const latest = computed(() => store.audit[0] ?? null)

function actionLabel(a: string): string {
  return (AUDIT_ACTION_LABELS as readonly string[]).includes(a) ? t(`remote.activity.actions.${a}`) : humanizeAction(a)
}

function reasonLabel(r: string | null): string {
  if (!r) return ''
  return (AUDIT_REASON_LABELS as readonly string[]).includes(r) ? t(`remote.activity.reasons.${r}`) : r
}

function resultLabel(e: AuditEntry): string {
  if (e.result === 'accepted') return t('remote.activity.accepted')
  if (e.result === 'rejected') return t('remote.activity.rejected')
  return e.result
}

function resultTone(r: string): 'success' | 'error' | 'neutral' {
  if (r === 'accepted') return 'success'
  if (r === 'rejected') return 'error'
  return 'neutral'
}

function fmtTime(ts: string | null): string {
  if (!ts) return '—'
  const d = new Date(ts)
  return Number.isNaN(d.getTime()) ? ts : d.toLocaleString()
}

function fmtShort(ts: string): string {
  const d = new Date(ts)
  if (Number.isNaN(d.getTime())) return ts
  const today = new Date().toDateString() === d.toDateString()
  return today ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : d.toLocaleDateString()
}

watch(open, (v) => {
  if (v) store.refreshAudit().catch(() => {})
})

async function clear() {
  const ok = await confirm({
    title: t('remote.activity.clearTitle'),
    message: t('remote.activity.clearMessage'),
    confirmLabel: t('remote.activity.clear'),
    variant: 'destructive',
  })
  if (!ok) return
  clearing.value = true
  try {
    const removed = await store.clearAudit()
    toast.success(removed > 0 ? t('remote.activity.cleared', { count: removed }) : t('remote.activity.alreadyEmpty'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    clearing.value = false
  }
}
</script>

<template>
  <Card>
    <div class="flex items-center gap-2 px-4 py-3" :class="open && 'border-b border-border/60 bg-muted/30'">
      <button
        type="button"
        class="flex min-w-0 flex-1 items-center gap-2 text-left cursor-pointer"
        :aria-expanded="open"
        @click="open = !open"
      >
        <ChevronRight class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform" :class="open && 'rotate-90'" :stroke-width="1.75" />
        <ShieldCheck class="h-4 w-4 shrink-0 text-primary" :stroke-width="1.75" />
        <span class="text-sm font-semibold">{{ t('remote.activity.title') }}</span>
        <span class="ml-auto min-w-0 truncate pl-3 text-[11px] text-muted-foreground">
          <template v-if="latest">
            {{ t('remote.activity.summary', { count: store.audit.length, at: fmtShort(latest.ts), action: actionLabel(latest.action) }) }}
          </template>
          <template v-else>{{ t('remote.activity.empty') }}</template>
        </span>
      </button>
      <template v-if="open">
        <Button
          variant="ghost"
          size="icon-sm"
          :title="t('common.refresh')"
          :aria-label="t('common.refresh')"
          @click="store.refreshAudit().catch(() => {})"
        >
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <Button
          variant="destructive-ghost"
          size="icon-sm"
          :title="t('remote.activity.clear')"
          :aria-label="t('remote.activity.clear')"
          :disabled="clearing || !store.audit.length"
          @click="clear"
        >
          <Loader2 v-if="clearing" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
          <Trash2 v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </template>
    </div>
    <template v-if="open">
      <div v-if="store.audit.length" class="max-h-80 overflow-auto">
        <table class="w-full text-xs">
          <thead class="sticky top-0 bg-muted/40 text-muted-foreground">
            <tr>
              <th class="px-3 py-2 text-left font-medium">{{ t('remote.activity.time') }}</th>
              <th class="px-3 py-2 text-left font-medium">{{ t('remote.activity.action') }}</th>
              <th class="px-3 py-2 text-left font-medium">{{ t('remote.activity.result') }}</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-border/40">
            <tr v-for="a in store.audit" :key="a.id">
              <td class="px-3 py-1.5 whitespace-nowrap text-muted-foreground">{{ fmtTime(a.ts) }}</td>
              <td class="px-3 py-1.5" :title="a.action">{{ actionLabel(a.action) }}</td>
              <td class="px-3 py-1.5">
                <Badge :tone="resultTone(a.result)" size="xs">{{ resultLabel(a) }}</Badge>
                <span v-if="a.reason" class="ml-1 text-muted-foreground/70">{{ reasonLabel(a.reason) }}</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-else class="px-4 py-6 text-center text-xs text-muted-foreground">
        {{ t('remote.activity.empty') }}
      </div>
    </template>
  </Card>
</template>
