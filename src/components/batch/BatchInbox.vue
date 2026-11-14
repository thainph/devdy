<script setup lang="ts">
// "Waiting for you" on the Batch Board: every permission prompt / question /
// plan the board's sessions wait on, in one place. A list on the left (who asks,
// what for, in one line), the selected prompt in full on the right — one prompt
// mounted at a time, so it gets the room of a drawer and keeps the keyboard
// (Enter allow, Esc deny…). Answering moves on to the next one.
import { computed, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Bot, ClipboardList, ExternalLink, HelpCircle, ShieldAlert } from 'lucide-vue-next'
import { Badge, Button, Card } from '@/components/ui'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useMarkdown } from '@/lib/markdown'
import { pendingSummary } from '@/lib/batchBoard'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'

export interface InboxEntry {
  runId: string
  projectId: string
  projectName: string
  /** The todo / session it belongs to. */
  name: string
  role: 'conductor' | 'worker' | 'session'
  /** The head of the run's queue: the one answered now. */
  request: PermissionRequest
  /** Requests queued behind it in the same run. */
  more: number
}

const props = defineProps<{ entries: InboxEntry[] }>()
/** Run id of the entry shown in full. */
const selected = defineModel<string>('selected', { default: '' })
const emit = defineEmits<{ open: [entry: { runId: string; projectId: string }] }>()

const { t } = useI18n()
const live = useLiveRunsStore()
const { renderText, loadMarkdown } = useMarkdown()
// Renders an ExitPlanMode plan inside the prompt; cached after the first load.
void loadMarkdown()

const current = computed(() => props.entries.find((e) => e.runId === selected.value) ?? props.entries[0] ?? null)
const total = computed(() => props.entries.reduce((n, e) => n + 1 + e.more, 0))

// Keep a valid selection: the answered run drops out (or moves to its next
// request) and the first one still waiting takes over.
watch(
  () => props.entries.map((e) => e.runId),
  (ids) => {
    if (!ids.includes(selected.value)) selected.value = ids[0] ?? ''
  },
  { immediate: true },
)

const KIND_ICON = { permission: ShieldAlert, question: HelpCircle, plan: ClipboardList } as const

function summary(e: InboxEntry) {
  return pendingSummary(e.request)
}

// Same answer path as the permission windows (PermissionWindowManager).
function decide(decision: 'allow' | 'deny', remember: boolean) {
  const e = current.value
  if (!e) return
  const req = e.request
  if (remember && decision === 'allow') live.rememberAllowedTool(e.runId, req.tool_name, req.request_id)
  else if (remember && decision === 'deny') live.rememberDeniedTool(e.runId, req.tool_name, req.request_id)
  void live.respondPermission(e.runId, req, decision, { remember })
}
function answer(answers: Record<string, string>) {
  const e = current.value
  if (e) void live.respondPermission(e.runId, e.request, 'allow', { answers })
}
</script>

<template>
  <Card body-class="p-0" class="ring-1 ring-warning/40">
    <template #header>
      <ShieldAlert class="h-3.5 w-3.5 text-warning" :stroke-width="1.75" />
      <span class="text-xs font-semibold">{{ t('board.inbox.title') }}</span>
      <span class="flex h-4 min-w-4 items-center justify-center rounded-full bg-warning px-1 text-[10px] font-medium leading-none text-warning-foreground tabular-nums">
        {{ total }}
      </span>
      <span class="ml-auto hidden truncate text-[11px] text-muted-foreground sm:block">{{ t('board.inbox.hint') }}</span>
    </template>

    <div class="grid md:h-[26rem] md:grid-cols-[minmax(14rem,19rem)_minmax(0,1fr)]">
      <!-- Who is waiting, and for what. -->
      <ul
        class="max-h-44 overflow-auto border-b border-border/60 md:max-h-none md:border-b-0 md:border-r"
        role="listbox"
        :aria-label="t('board.inbox.title')"
      >
        <li v-for="e in entries" :key="e.runId">
          <button
            type="button"
            role="option"
            :aria-selected="current?.runId === e.runId"
            class="flex w-full items-start gap-2 border-l-2 px-3 py-2 text-left transition-colors cursor-pointer"
            :class="current?.runId === e.runId ? 'border-l-primary bg-primary/10' : 'border-l-transparent hover:bg-accent/50'"
            @click="selected = e.runId"
          >
            <component
              :is="KIND_ICON[summary(e).kind]"
              class="mt-0.5 h-3.5 w-3.5 shrink-0"
              :class="summary(e).kind === 'permission' ? 'text-warning' : 'text-info'"
              :stroke-width="1.75"
            />
            <span class="min-w-0 flex-1 space-y-0.5">
              <span class="flex min-w-0 items-center gap-1.5">
                <Badge size="xs" tone="neutral" class="max-w-[45%] shrink-0 truncate">{{ e.projectName }}</Badge>
                <Bot v-if="e.role === 'conductor'" class="h-3 w-3 shrink-0 text-primary" :stroke-width="1.75" />
                <span class="truncate text-xs font-medium">{{ e.name }}</span>
              </span>
              <span class="block truncate text-[11px] text-muted-foreground">
                <template v-if="summary(e).kind === 'permission'">
                  <span class="font-mono text-foreground/80">{{ summary(e).tool }}</span>
                  <template v-if="summary(e).detail"> · <span class="font-mono">{{ summary(e).detail }}</span></template>
                </template>
                <template v-else>
                  {{ t(`board.inbox.${summary(e).kind}`) }}<template v-if="summary(e).detail">: {{ summary(e).detail }}</template>
                </template>
              </span>
            </span>
            <span
              v-if="e.more"
              class="mt-0.5 shrink-0 rounded-full bg-muted px-1.5 text-[10px] tabular-nums text-muted-foreground"
              :title="t('board.morePending', { n: e.more })"
            >+{{ e.more }}</span>
          </button>
        </li>
      </ul>

      <!-- The selected prompt, in full. -->
      <div v-if="current" class="flex min-h-0 flex-col">
        <div class="flex shrink-0 items-center gap-2 px-4 py-2 text-[11px] text-muted-foreground">
          <Bot v-if="current.role === 'conductor'" class="h-3.5 w-3.5 shrink-0 text-primary" :stroke-width="1.75" />
          <span class="min-w-0 truncate">
            <span class="font-medium text-foreground">{{ current.name }}</span>
            · {{ t(`board.inbox.role.${current.role}`) }} · {{ current.projectName }}
          </span>
          <span v-if="current.more" class="shrink-0">· {{ t('board.morePending', { n: current.more }) }}</span>
          <Button variant="ghost" size="xs" class="ml-auto shrink-0" @click="emit('open', current)">
            <ExternalLink class="h-3 w-3" :stroke-width="2" />{{ t('board.openSession') }}
          </Button>
        </div>
        <div class="h-[24rem] min-h-0 md:h-auto md:flex-1">
          <PermissionPrompt
            :key="current.request.request_id"
            :request="current.request"
            :allowed-tools="live.get(current.runId)?.allowedTools ?? []"
            :render-text="renderText"
            @decide="decide"
            @answer="answer"
          />
        </div>
      </div>
    </div>
  </Card>
</template>
