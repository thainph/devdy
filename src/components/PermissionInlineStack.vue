<script setup lang="ts">
// In-app answers for pending permission / question requests while the main window
// is in front (mounted once in the main window, next to PermissionWindowManager —
// which closes the standalone permission windows meanwhile).
//
// A stack of cards anchored bottom-right, above content and below modals: the
// front card holds the full shared PermissionPrompt (allow / always / deny, plan
// review, the AskUserQuestion form); up to two more show a one-line summary and
// come to the front on click. Every answer goes through usePermissionDecisions, the
// single writer. Hiding a card does NOT deny: the request stays pending and is
// counted in the chip on top of the stack (and in the title-bar pill).
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  ChevronDown,
  ChevronUp,
  ClipboardList,
  ExternalLink,
  HelpCircle,
  Minus,
  ShieldAlert,
} from 'lucide-vue-next'
import { Button } from '@/components/ui'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useMarkdown } from '@/lib/markdown'
import { stackLayout, summarizePermissionInput } from '@/lib/permissionSurface'
import {
  registerStackHost,
  usePermissionDecisions,
  type PendingPermission,
} from '@/composables/usePermissionDecisions'

const { t } = useI18n()
const live = useLiveRunsStore()
const perms = usePermissionDecisions()
const { renderText, loadMarkdown } = useMarkdown()

const MAX_VISIBLE = 3

// Requests docked in a split RunView or popped out into a window aren't stacked.
const layout = computed(() =>
  stackLayout(perms.stackEntries.value, perms.collapsed.value, perms.promoted.value, MAX_VISIBLE),
)
const front = computed(() => layout.value.visible[0] ?? null)
// Shown top → bottom above the front card, so the next one sits right on top of it.
const behind = computed(() => layout.value.visible.slice(1).reverse())
const hidden = computed(() => [...layout.value.overflow, ...layout.value.collapsed])
const show = computed(() => perms.surface.value === 'inline' && perms.stackEntries.value.length > 0)

const hiddenOpen = ref(false)
watch(
  () => hidden.value.length,
  (n) => {
    if (!n) hiddenOpen.value = false
  },
)
watch(perms.entries, () => perms.prune())

function sessionLine(e: PendingPermission) {
  return perms.sessionOf(e.runId)
}

function kindIcon(e: PendingPermission) {
  if (e.request.tool_name === 'AskUserQuestion') return HelpCircle
  if (e.request.tool_name === 'ExitPlanMode') return ClipboardList
  return ShieldAlert
}

function kindTone(e: PendingPermission) {
  const info = e.request.tool_name === 'AskUserQuestion' || e.request.tool_name === 'ExitPlanMode'
  return info ? 'text-info' : 'text-warning'
}

function toolLabel(e: PendingPermission): string {
  if (e.request.tool_name === 'AskUserQuestion') return t('permission.inline.question')
  if (e.request.tool_name === 'ExitPlanMode') return t('permission.inline.plan')
  return e.request.display_name || e.request.tool_name
}

function summary(e: PendingPermission): string {
  return summarizePermissionInput(e.request.tool_name, e.request.tool_input)
}

function onDecide(e: PendingPermission, decision: 'allow' | 'deny', remember: boolean) {
  void perms.decide(e.runId, e.requestId, decision, remember)
}

function onAnswer(e: PendingPermission, answers: Record<string, string>) {
  void perms.answerQuestion(e.runId, e.requestId, answers)
}

function bring(e: PendingPermission) {
  perms.bringToFront(e.runId)
  hiddenOpen.value = false
}

// Focus requests from the title-bar pill: bring the card forward (done by
// focusRun) and move keyboard focus into its prompt.
const promptRef = ref<InstanceType<typeof PermissionPrompt> | null>(null)
watch(perms.focusRequest, async (req) => {
  // A run docked in a RunView focuses its own prompt there.
  if (!req || perms.dockedRuns.value.has(req.runId)) return
  await nextTick()
  promptRef.value?.focus()
})

let unregister: (() => void) | null = null
onMounted(() => {
  unregister = registerStackHost()
  loadMarkdown()
})
onBeforeUnmount(() => unregister?.())
</script>

<template>
  <div
    v-if="show"
    class="pointer-events-none fixed bottom-4 right-4 z-40 flex w-[420px] max-w-[calc(100vw-2rem)] flex-col items-stretch gap-2"
    role="region"
    :aria-label="t('permission.inline.regionLabel')"
  >
    <!-- Requests not on screen: beyond the visible limit, or hidden by the user. -->
    <div v-if="hidden.length" class="pointer-events-auto flex flex-col items-end gap-1">
      <button
        type="button"
        class="inline-flex items-center gap-1 rounded-full border border-warning/30 bg-warning/15 px-2.5 py-1 text-[10px] font-medium text-warning shadow-md hover:bg-warning/25 cursor-pointer"
        :aria-expanded="hiddenOpen"
        @click="hiddenOpen = !hiddenOpen"
      >
        <ShieldAlert class="h-3 w-3" :stroke-width="2" />
        {{
          front
            ? t('permission.inline.more', { n: hidden.length })
            : t('permission.inline.waiting', { n: hidden.length })
        }}
        <component :is="hiddenOpen ? ChevronDown : ChevronUp" class="h-3 w-3" :stroke-width="2" />
      </button>
      <ul
        v-if="hiddenOpen"
        class="ui-popover w-full max-h-60 overflow-auto rounded-lg border border-border bg-popover p-1 shadow-lg"
      >
        <li v-for="e in hidden" :key="e.requestId">
          <button
            type="button"
            class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left hover:bg-accent/50 cursor-pointer"
            @click="bring(e)"
          >
            <component :is="kindIcon(e)" class="h-3.5 w-3.5 shrink-0" :class="kindTone(e)" :stroke-width="1.75" />
            <span class="min-w-0 flex-1">
              <span class="block truncate text-xs text-foreground">{{ sessionLine(e).name }}</span>
              <span class="block truncate text-[11px] text-muted-foreground">
                <span class="font-mono">{{ toolLabel(e) }}</span>
                <template v-if="summary(e)"> · {{ summary(e) }}</template>
              </span>
            </span>
          </button>
        </li>
      </ul>
    </div>

    <!-- Cards behind the front one: a one-line summary, click to bring forward. -->
    <div
      v-for="e in behind"
      :key="e.requestId"
      class="ui-card pointer-events-auto flex items-center gap-2 rounded-lg border border-border bg-card px-3 py-2 shadow-md hover:bg-accent/50 cursor-pointer"
      role="button"
      tabindex="0"
      :title="t('permission.inline.bringToFront')"
      @click="bring(e)"
      @keydown.enter.self.prevent="bring(e)"
      @keydown.space.self.prevent="bring(e)"
    >
      <component :is="kindIcon(e)" class="h-3.5 w-3.5 shrink-0" :class="kindTone(e)" :stroke-width="1.75" />
      <span class="min-w-0 flex-1">
        <span class="block truncate text-xs font-medium text-foreground">
          {{ sessionLine(e).name }}
          <span v-if="sessionLine(e).project && sessionLine(e).project !== sessionLine(e).name" class="font-normal text-muted-foreground">
            · {{ sessionLine(e).project }}
          </span>
        </span>
        <span class="block truncate text-[11px] text-muted-foreground">
          <span class="font-mono">{{ toolLabel(e) }}</span>
          <template v-if="summary(e)"> · {{ summary(e) }}</template>
        </span>
      </span>
      <Button
        variant="ghost"
        size="icon-xs"
        :title="t('permission.inline.hide')"
        :aria-label="t('permission.inline.hide')"
        @click.stop="perms.collapse(e.requestId)"
      >
        <Minus class="h-3 w-3" :stroke-width="2" />
      </Button>
    </div>

    <!-- Front card: who is asking + the full shared prompt. Its Enter / Esc
         hotkeys (allow / deny) are handled by the prompt and stop here, so they
         never also reach app-wide shortcuts. -->
    <div
      v-if="front"
      :key="`front-${front.runId}`"
      @keydown.enter.stop
      @keydown.esc.stop
      class="ui-card pointer-events-auto flex max-h-[min(560px,calc(100vh-7rem))] flex-col overflow-hidden rounded-lg border border-border bg-card shadow-lg"
    >
      <div class="flex shrink-0 items-center gap-2 border-b border-border/60 px-3 py-1.5">
        <div class="flex min-w-0 flex-1 flex-col leading-tight">
          <span
            v-if="sessionLine(front).project && sessionLine(front).project !== sessionLine(front).name"
            class="truncate text-[10px] uppercase tracking-wide text-muted-foreground"
            :title="sessionLine(front).project"
          >
            {{ sessionLine(front).project }}
          </span>
          <span class="truncate text-xs font-medium text-foreground" :title="sessionLine(front).name">
            {{ sessionLine(front).name }}
          </span>
        </div>
        <Button variant="outline" size="xs" class="shrink-0" @click="perms.openSession(front.runId)">
          <ExternalLink class="h-3 w-3" :stroke-width="2" />
          {{ t('permission.inline.openSession') }}
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          class="shrink-0"
          :title="t('permission.inline.hide')"
          :aria-label="t('permission.inline.hide')"
          @click="perms.collapse(front.requestId)"
        >
          <Minus class="h-3 w-3" :stroke-width="2" />
        </Button>
      </div>
      <PermissionPrompt
        ref="promptRef"
        :key="front.requestId"
        class="min-h-0 flex-1"
        :request="front.request"
        :allowed-tools="live.get(front.runId)?.allowedTools ?? []"
        :render-text="renderText"
        no-autofocus
        @decide="(d, r) => onDecide(front!, d, r)"
        @answer="(a) => onAnswer(front!, a)"
      />
    </div>
  </div>
</template>
