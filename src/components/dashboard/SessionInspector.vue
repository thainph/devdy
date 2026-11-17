<script setup lang="ts">
// Runtime Dashboard — the sticky detail panel of the selected session: a live
// log (fixed height, follows the bottom while you are there) and an action
// zone of fixed minimum height:
//   • a permission prompt → Allow / Always allow / Deny, through the same
//     liveRuns path the permission windows use (`respondPermission`, Always =
//     the project's standing allow list);
//   • an AskUserQuestion → suggested answers + a free answer, sent as the
//     same `answers` map the permission window sends;
//   • otherwise → message the session (`send_user_message`) or stop it.
// After an answer it reports `answered` so the dashboard moves on to the next
// session waiting for you; the short confirmation stays visible here.
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronLeft, ChevronRight, ExternalLink, MessageCircleQuestion, ShieldAlert, Square } from 'lucide-vue-next'
import { Button, Textarea } from '@/components/ui'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useConfirm } from '@/composables/useConfirm'
import { useSessionLog } from '@/composables/useRuntimeDashboard'
import { formatCpu, formatRss } from '@/lib/backgroundProcesses'
import { toolLabel, type LogKind, type RtSession } from '@/lib/runtimeDashboard'
import type { ExternalSession } from '@/lib/runtimeSnapshot'
import EngineMark from './EngineMark.vue'
import { STATE_TAG, useRuntimeLabels } from './runtimeStyles'

const props = defineProps<{
  session: RtSession | null
  /** Set when the session runs outside Devdy. */
  external: ExternalSession | null
  /** Waiting-for-you queue (ids), longest first. */
  queue: string[]
  now: number
}>()
const emit = defineEmits<{
  prev: []
  next: []
  /** A prompt of this session was answered (move on to the next waiting one). */
  answered: [id: string]
  open: [session: RtSession]
  openTranscript: [path: string]
}>()

const { t } = useI18n()
const L = useRuntimeLabels()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const { confirm } = useConfirm()

const s = computed(() => props.session)
const runId = computed(() => s.value?.runId ?? null)

// ── The run whose log is shown (an external session: its mirrored run) ────
const mirroredRunId = computed(() => {
  const e = props.external
  if (!e) return null
  for (const r of runsStore.runMeta.values()) if (r.session_id === e.id) return r.id
  return null
})
watch(
  () => props.external?.id,
  () => {
    const e = props.external
    if (e?.projectId && !mirroredRunId.value) runsStore.refreshMeta(e.projectId).catch(() => {})
  },
  { immediate: true },
)
const logRunId = computed(() => runId.value ?? mirroredRunId.value)
const log = useSessionLog(logRunId)

const SYMBOL: Record<LogKind, string> = { tool: '›', ok: '✓', ask: '?', err: '✗', text: '·', user: '»', note: '–' }
const TONE: Record<LogKind, string> = {
  tool: 'text-primary',
  ok: 'text-success',
  ask: 'text-warning',
  err: 'text-destructive',
  text: 'text-foreground/90',
  user: 'text-info',
  note: 'text-muted-foreground',
}
function hm(at: number | null): string {
  return at === null ? '' : new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}

// Follow the bottom only while the reader is there.
const logEl = ref<HTMLElement | null>(null)
const stick = ref(true)
function onLogScroll() {
  const el = logEl.value
  if (el) stick.value = el.scrollHeight - el.scrollTop - el.clientHeight < 24
}
function toBottom() {
  void nextTick(() => {
    const el = logEl.value
    if (el) el.scrollTop = el.scrollHeight
  })
}
watch(() => log.lines.value.length, () => { if (stick.value) toBottom() })
watch(logRunId, () => {
  stick.value = true
  toBottom()
})

// ── Prompt of the selected run ────────────────────────────────────────────
const head = computed(() => (runId.value ? live.sessions.get(runId.value)?.permissionQueue[0] ?? null : null))
const isQuestion = computed(() => head.value?.tool_name === 'AskUserQuestion')
const isPlan = computed(() => head.value?.tool_name === 'ExitPlanMode')
/** Same rule as the permission window: no Always for plans, bare MCP or broker prompts. */
const canRemember = computed(() => !!head.value && !isPlan.value && head.value.tool_name !== 'MCP' && !head.value.once_only)

function obj(v: unknown): Record<string, unknown> {
  return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
}
/** The full command / target of the prompt. */
const promptBody = computed(() => {
  const h = head.value
  if (!h) return ''
  const input = obj(h.tool_input)
  const cmd = input.command ?? input.cmd
  if (typeof cmd === 'string') return cmd
  if (Array.isArray(cmd)) return cmd.join(' ')
  if (isPlan.value && typeof input.plan === 'string') return input.plan
  for (const k of ['file_path', 'path', 'url', 'pattern', 'query']) if (typeof input[k] === 'string') return input[k] as string
  if (h.description) return h.description
  const json = JSON.stringify(h.tool_input ?? null, null, 2)
  return json === 'null' ? '' : json
})

interface Question { question: string; multiSelect: boolean; options: string[] }
const questions = computed<Question[]>(() => {
  if (!isQuestion.value) return []
  const raw = obj(head.value?.tool_input).questions
  if (!Array.isArray(raw)) return []
  return raw.map((q) => {
    const o = obj(q)
    const opts = Array.isArray(o.options) ? o.options.map((x) => String(obj(x).label ?? '')).filter(Boolean) : []
    return { question: String(o.question ?? ''), multiSelect: o.multiSelect === true, options: opts }
  })
})
const step = ref(0)
/** Options picked for each question (the current one is still editable). */
const picked = ref<string[][]>([])
/** Final answer of each question already passed. */
const finals = ref<string[]>([])
const freeText = ref('')
const message = ref('')
watch(() => head.value?.request_id, () => {
  step.value = 0
  picked.value = []
  finals.value = []
  freeText.value = ''
})
const current = computed(() => questions.value[step.value] ?? null)

// ── Flash (kept briefly, also after moving on to another session) ──────────
const flash = ref<{ text: string; tone: 'ok' | 'bad'; title: string } | null>(null)
let flashTimer: ReturnType<typeof setTimeout> | null = null
function showFlash(text: string, tone: 'ok' | 'bad') {
  flash.value = { text, tone, title: s.value?.title ?? '' }
  if (flashTimer) clearTimeout(flashTimer)
  flashTimer = setTimeout(() => (flash.value = null), 4000)
}
onBeforeUnmount(() => {
  if (flashTimer) clearTimeout(flashTimer)
})

const busy = ref(false)

async function decide(decision: 'allow' | 'deny', remember = false) {
  const req = head.value
  const id = runId.value
  const sid = s.value?.id
  if (!req || !id || !sid || busy.value || isQuestion.value) return
  if (remember && !canRemember.value) return
  busy.value = true
  try {
    if (remember) live.rememberAllowedTool(id, req.tool_name, req.request_id)
    const ok = await live.respondPermission(id, req, decision, { remember })
    if (!ok) {
      showFlash(t('dashboard.detail.flash.failed'), 'bad')
      return
    }
    showFlash(
      decision === 'deny' ? t('dashboard.detail.flash.denied') : remember ? t('dashboard.detail.flash.always') : t('dashboard.detail.flash.allowed'),
      decision === 'deny' ? 'bad' : 'ok',
    )
    emit('answered', sid)
  } finally {
    busy.value = false
  }
}

function answerOf(i: number): string {
  const parts = [...(picked.value[i] ?? [])]
  if (i === step.value && freeText.value.trim()) parts.push(freeText.value.trim())
  return parts.join(', ')
}

async function submitAnswers(answers: Record<string, string>) {
  const req = head.value
  const id = runId.value
  const sid = s.value?.id
  if (!req || !id || !sid || busy.value) return
  busy.value = true
  try {
    const ok = await live.respondPermission(id, req, 'allow', { answers })
    if (!ok) {
      showFlash(t('dashboard.detail.flash.failed'), 'bad')
      return
    }
    showFlash(t('dashboard.detail.flash.answered'), 'ok')
    emit('answered', sid)
  } finally {
    busy.value = false
  }
}

/** Record the current question's answer, then go on or send them all. */
function advance() {
  if (!current.value) return
  const answer = answerOf(step.value)
  if (!answer) return
  const next = [...finals.value]
  next[step.value] = answer
  finals.value = next
  freeText.value = ''
  if (step.value < questions.value.length - 1) {
    step.value++
    return
  }
  const answers: Record<string, string> = {}
  questions.value.forEach((q, i) => {
    answers[q.question] = finals.value[i] ?? ''
  })
  void submitAnswers(answers)
}

function pickOption(label: string) {
  const q = current.value
  if (!q) return
  const cur = picked.value[step.value] ?? []
  const next = [...picked.value]
  if (q.multiSelect) {
    next[step.value] = cur.includes(label) ? cur.filter((l) => l !== label) : [...cur, label]
    picked.value = next
    return
  }
  next[step.value] = [label]
  picked.value = next
  advance()
}

function onAnswerKey(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault()
    advance()
  }
}

// ── Message / stop ────────────────────────────────────────────────────────
async function send() {
  const text = message.value.trim()
  const cur = s.value
  if (!text || !cur?.runId || !cur.projectId || busy.value) return
  busy.value = true
  try {
    live.pushUser(cur.runId, cur.projectId, text)
    await runsStore.sendUserMessage(cur.runId, text)
    message.value = ''
    showFlash(t('dashboard.detail.flash.sent'), 'ok')
  } catch {
    showFlash(t('dashboard.detail.flash.failed'), 'bad')
  } finally {
    busy.value = false
  }
}
function onMessageKey(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault()
    void send()
  }
}

async function stop() {
  const cur = s.value
  if (!cur?.runId) return
  const ok = await confirm({
    title: t('dashboard.detail.stopTitle'),
    message: t('dashboard.detail.stopMessage', { name: cur.title }),
    confirmLabel: t('dashboard.detail.stop'),
    variant: 'destructive',
  })
  if (!ok) return
  try {
    await runsStore.cancelRun(cur.runId)
    showFlash(t('dashboard.detail.flash.stopped'), 'ok')
  } catch {
    showFlash(t('dashboard.detail.flash.failed'), 'bad')
  }
}

// ── Header bits ───────────────────────────────────────────────────────────
const queueIndex = computed(() => (s.value ? props.queue.indexOf(s.value.id) : -1))
const queueLabel = computed(() =>
  queueIndex.value >= 0
    ? t('dashboard.detail.queue', { i: queueIndex.value + 1, n: props.queue.length })
    : t('dashboard.detail.queueCount', { n: props.queue.length }),
)
const metaLine = computed(() => {
  const cur = s.value
  if (!cur) return ''
  const parts = [L.meta(cur), L.elapsed(cur, props.now)]
  if (cur.cpuPercent !== null && cur.rssKb !== null) {
    parts.push(t('dashboard.detail.resources', { cpu: formatCpu(cur.cpuPercent), ram: formatRss(cur.rssKb) }))
  }
  return parts.filter(Boolean).join(' · ')
})
const hint = computed(() => {
  const cur = s.value
  if (!cur) return ''
  if (cur.state === 'external') return t('dashboard.detail.hintExternal')
  if (cur.state === 'idle') return t('dashboard.detail.hintIdle')
  return t('dashboard.detail.hintCalm')
})
const flashForOther = computed(() => !!flash.value && flash.value.title !== (s.value?.title ?? ''))

defineExpose({
  /** Keyboard `a`: allow the open permission prompt (never a question). */
  allow: () => decide('allow'),
  /** Keyboard `d`: deny it. */
  deny: () => decide('deny'),
})
</script>

<template>
  <aside class="ui-card glass flex flex-col gap-3 rounded-lg border border-border/70 p-3.5 shadow-lg shadow-black/5" :aria-label="t('dashboard.detail.title')">
    <div class="flex items-center gap-2">
      <span class="flex-1 text-[10px] font-bold tracking-wider text-muted-foreground uppercase">{{ t('dashboard.detail.title') }}</span>
      <span v-if="queue.length" class="rounded-full bg-warning/15 px-2 py-0.5 text-[10px] font-semibold text-warning">{{ queueLabel }}</span>
      <Button variant="outline" size="icon-sm" :disabled="!queue.length" :title="t('dashboard.detail.prev')" :aria-label="t('dashboard.detail.prev')" @click="emit('prev')">
        <ChevronLeft class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button variant="outline" size="icon-sm" :disabled="!queue.length" :title="t('dashboard.detail.next')" :aria-label="t('dashboard.detail.next')" @click="emit('next')">
        <ChevronRight class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </div>

    <p v-if="!s" class="py-10 text-center text-xs text-muted-foreground">{{ t('dashboard.detail.none') }}</p>
    <template v-else>
      <div class="flex min-w-0 items-center gap-2.5">
        <EngineMark :engine="s.engine" size="lg" />
        <div class="min-w-0 flex-1">
          <div class="truncate text-[15px] font-bold" :title="s.title">{{ s.title }}</div>
          <div class="truncate text-[11px] text-muted-foreground" :title="metaLine">{{ metaLine }}</div>
        </div>
        <span class="shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold" :class="STATE_TAG[s.state]">{{ L.stateLabel(s.state) }}</span>
      </div>

      <div
        ref="logEl"
        class="flex h-[300px] flex-col gap-0.5 overflow-y-auto rounded-md border border-border/60 bg-muted/40 px-2.5 py-2 font-mono text-[11px] leading-relaxed"
        role="log"
        aria-live="polite"
        :aria-label="t('dashboard.detail.logLabel', { name: s.title })"
        @scroll.passive="onLogScroll"
      >
        <p v-if="!log.lines.value.length" class="m-auto text-center text-muted-foreground">
          {{ log.loading.value ? t('dashboard.detail.logLoading') : external && !logRunId ? t('dashboard.detail.externalNoLog') : t('dashboard.detail.logEmpty') }}
        </p>
        <div v-for="l in log.lines.value" :key="l.key" class="grid grid-cols-[38px_12px_minmax(0,1fr)] items-start gap-1.5">
          <span class="text-muted-foreground/70 tabular-nums">{{ hm(l.at) }}</span>
          <span :class="TONE[l.kind]" aria-hidden="true">{{ SYMBOL[l.kind] }}</span>
          <span class="break-words whitespace-pre-wrap" :class="TONE[l.kind]">{{ l.text || (l.kind === 'ok' ? t('dashboard.detail.turnDone') : '') }}</span>
        </div>
      </div>

      <div class="flex min-h-[170px] flex-col gap-2 border-t border-border/60 pt-3">
        <p
          v-if="flash"
          class="text-xs font-semibold"
          :class="flash.tone === 'bad' ? 'text-destructive' : 'text-success'"
          role="status"
        >
          <span v-if="flashForOther" class="text-muted-foreground">{{ flash.title }} · </span>{{ flash.text }}
        </p>

        <!-- Permission prompt -->
        <template v-if="head && !isQuestion">
          <div class="flex items-center gap-1.5 text-[13px] font-semibold">
            <ShieldAlert class="h-3.5 w-3.5 shrink-0 text-warning" :stroke-width="2" aria-hidden="true" />
            <span class="truncate">{{ isPlan ? t('dashboard.detail.planTitle') : t('dashboard.detail.permTitle', { tool: toolLabel(head.tool_name) }) }}</span>
          </div>
          <code v-if="promptBody" class="block max-h-24 overflow-auto rounded-md border border-border/60 bg-muted/50 px-2 py-1.5 font-mono text-[11px] break-all whitespace-pre-wrap">{{ promptBody }}</code>
          <div class="grid gap-1.5" :class="canRemember ? 'grid-cols-3' : 'grid-cols-2'">
            <Button variant="primary" size="sm" :disabled="busy" @click="decide('allow')">
              {{ isPlan ? t('dashboard.detail.approve') : t('dashboard.detail.allow') }}
            </Button>
            <Button v-if="canRemember" variant="outline" size="sm" :disabled="busy" :title="t('dashboard.detail.alwaysTitle')" @click="decide('allow', true)">
              {{ t('dashboard.detail.always') }}
            </Button>
            <Button variant="destructive" size="sm" :disabled="busy" @click="decide('deny')">
              {{ isPlan ? t('dashboard.detail.keepPlanning') : t('dashboard.detail.deny') }}
            </Button>
          </div>
        </template>

        <!-- AskUserQuestion -->
        <template v-else-if="head && current">
          <div class="flex items-start gap-1.5 text-[13px] font-semibold">
            <MessageCircleQuestion class="mt-0.5 h-3.5 w-3.5 shrink-0 text-warning" :stroke-width="2" aria-hidden="true" />
            <span class="min-w-0 flex-1">
              <span v-if="questions.length > 1" class="mr-1 text-[11px] font-medium text-muted-foreground">{{ t('dashboard.detail.questionStep', { i: step + 1, n: questions.length }) }}</span>
              {{ current.question }}
            </span>
          </div>
          <div v-if="current.options.length" class="flex flex-wrap gap-1.5">
            <Button
              v-for="o in current.options"
              :key="o"
              :variant="(picked[step] ?? []).includes(o) ? 'primary' : 'outline'"
              size="xs"
              :disabled="busy"
              :aria-pressed="(picked[step] ?? []).includes(o)"
              @click="pickOption(o)"
            >
              {{ o }}
            </Button>
          </div>
          <div class="flex items-end gap-1.5">
            <label class="flex flex-1">
              <span class="sr-only">{{ t('dashboard.detail.answerLabel') }}</span>
              <Textarea v-model="freeText" rows="1" class="min-h-8 resize-none" :placeholder="t('dashboard.detail.answerPlaceholder')" @keydown="onAnswerKey" />
            </label>
            <Button variant="primary" size="sm" :disabled="busy || !answerOf(step)" @click="advance">{{ t('dashboard.detail.send') }}</Button>
          </div>
        </template>

        <!-- Nothing to answer -->
        <template v-else>
          <p class="text-xs text-muted-foreground">{{ hint }}</p>
          <div v-if="s.state !== 'external'" class="flex items-end gap-1.5">
            <label class="flex flex-1">
              <span class="sr-only">{{ t('dashboard.detail.msgLabel') }}</span>
              <Textarea v-model="message" rows="1" class="min-h-8 resize-none" :placeholder="t('dashboard.detail.msgPlaceholder')" @keydown="onMessageKey" />
            </label>
            <Button variant="outline" size="sm" :disabled="busy || !message.trim()" @click="send">{{ t('dashboard.detail.send') }}</Button>
          </div>
        </template>

        <div class="mt-auto flex flex-wrap gap-1.5 pt-1">
          <Button v-if="s.runId" variant="outline" size="sm" @click="emit('open', s)">
            <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('dashboard.detail.open') }}
          </Button>
          <Button v-else-if="external" variant="outline" size="sm" @click="emit('openTranscript', external.transcriptPath)">
            <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />{{ t('dashboard.detail.openTranscript') }}
          </Button>
          <Button v-if="s.runId" variant="destructive" size="sm" class="ml-auto" @click="stop">
            <Square class="h-3 w-3" :stroke-width="2" />{{ t('dashboard.detail.stop') }}
          </Button>
        </div>
      </div>
    </template>
  </aside>
</template>
