<script setup lang="ts">
// One worker of a todo batch on the Batch Board: its todo, branch and diff, what
// it did last, and the actions of its lane. A pending permission prompt /
// question is answered right on the card; to chat with it, open its session.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertTriangle,
  Check,
  ExternalLink,
  GitBranch,
  GitCommitHorizontal,
  Square,
  Trash2,
  Upload,
} from 'lucide-vue-next'
import { Badge, Button, Card } from '@/components/ui'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useMarkdown } from '@/lib/markdown'
import { diffParts, lastActivity, roleLabel, type BoardItem, type Lane } from '@/lib/batchBoard'

const props = defineProps<{
  item: BoardItem
  lane: Lane
  /** An action on this card is in flight. */
  busy?: boolean
  /** Name the project (solo sessions; a batch's section already does). */
  showProject?: boolean
}>()

const emit = defineEmits<{
  open: []
  stop: []
  commit: []
  push: []
  finish: []
  discard: []
}>()

const { t } = useI18n()
const live = useLiveRunsStore()
const { renderText, loadMarkdown } = useMarkdown()
// Renders an ExitPlanMode plan inside the prompt; cached after the first load.
void loadMarkdown()

const session = computed(() => live.get(props.item.runId))
const head = computed(() => session.value?.permissionQueue[0] ?? null)
const morePending = computed(() => Math.max(0, (session.value?.permissionQueue.length ?? 0) - 1))
const activity = computed(() => lastActivity(session.value?.entries ?? []))
const attention = computed(() => session.value?.attention[0] ?? null)
const role = computed(() => roleLabel(props.item.title))
const heading = computed(() => {
  const text = props.item.todo?.text ?? props.item.title?.replace(/^\[[^\]]*\]\s*/, '') ?? ''
  return text.split('\n')[0].trim() || t('board.untitled')
})
const diff = computed(() => diffParts(props.item.diff))
const hasWorktree = computed(() => !!props.item.worktree && !props.item.worktree.removedAt)
const canPush = computed(() => hasWorktree.value && (props.item.diff?.commits ?? 0) > 0)
const canCommit = computed(() => hasWorktree.value && (props.item.diff?.uncommitted ?? 0) > 0)

// Same answer path as the permission windows (PermissionWindowManager).
function decide(decision: 'allow' | 'deny', remember: boolean) {
  const req = head.value
  if (!req) return
  if (remember && decision === 'allow') live.rememberAllowedTool(props.item.runId, req.tool_name, req.request_id)
  else if (remember && decision === 'deny') live.rememberDeniedTool(props.item.runId, req.tool_name, req.request_id)
  void live.respondPermission(props.item.runId, req, decision, { remember })
}

function answer(answers: Record<string, string>) {
  const req = head.value
  if (req) void live.respondPermission(props.item.runId, req, 'allow', { answers })
}
</script>

<template>
  <Card body-class="p-3 space-y-2" :class="lane === 'needs' ? 'ring-1 ring-primary/50' : ''">
    <div v-if="showProject || role || item.costUsd > 0" class="flex items-center gap-1.5 min-w-0">
      <Badge v-if="showProject" size="xs" tone="neutral" class="shrink-0 max-w-[45%] truncate" :title="item.projectName">{{ item.projectName }}</Badge>
      <span v-if="role" class="truncate text-[10px] text-muted-foreground">{{ role }}</span>
      <span v-if="item.costUsd > 0" class="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground">${{ item.costUsd.toFixed(2) }}</span>
    </div>

    <button
      type="button"
      class="block w-full text-left text-[13px] font-medium leading-snug line-clamp-2 hover:text-primary cursor-pointer"
      :title="t('board.openSession')"
      @click="emit('open')"
    >
      {{ heading }}
    </button>

    <div v-if="item.worktree || diff" class="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] text-muted-foreground">
      <span v-if="item.worktree" class="flex min-w-0 items-center gap-1 font-mono" :title="item.worktree.path">
        <GitBranch class="h-3 w-3 shrink-0" :stroke-width="1.75" />
        <span class="truncate">{{ item.worktree.branch }}</span>
      </span>
      <template v-if="diff">
        <span class="font-mono"><span class="text-success">+{{ diff.insertions }}</span> <span class="text-destructive">−{{ diff.deletions }}</span></span>
        <span>{{ t('board.files', { n: diff.files }, diff.files) }}</span>
        <span>{{ t('board.commits', { n: diff.commits }, diff.commits) }}</span>
        <span v-if="diff.uncommitted" class="text-warning">{{ t('board.uncommitted', { n: diff.uncommitted }, diff.uncommitted) }}</span>
      </template>
    </div>

    <p v-if="activity && lane !== 'needs'" class="text-[11px] text-muted-foreground line-clamp-2" :title="activity">{{ activity }}</p>

    <div v-if="lane === 'alert' && attention" class="flex items-start gap-1.5 text-[11px] text-warning">
      <AlertTriangle class="h-3 w-3 mt-0.5 shrink-0" :stroke-width="2" />
      <span>{{ attention.message }}</span>
    </div>

    <!-- The session is blocked on the user: answer here. -->
    <div v-if="head" class="-mx-3 border-y border-border/60 bg-muted/30">
      <PermissionPrompt
        :key="head.request_id"
        :request="head"
        :allowed-tools="session?.allowedTools ?? []"
        :render-text="renderText"
        @decide="decide"
        @answer="answer"
      />
      <p v-if="morePending" class="px-3 pb-2 text-[11px] text-muted-foreground">{{ t('board.morePending', { n: morePending }) }}</p>
    </div>

    <div class="flex flex-wrap items-center gap-1 pt-0.5">
      <Button variant="outline" size="xs" @click="emit('open')">
        <ExternalLink class="h-3 w-3" :stroke-width="2" />{{ t('board.open') }}
      </Button>
      <template v-if="lane === 'running' || lane === 'alert' || lane === 'needs'">
        <Button variant="destructive-ghost" size="xs" :disabled="busy" @click="emit('stop')">
          <Square class="h-3 w-3" :stroke-width="2" />{{ t('board.stop') }}
        </Button>
      </template>
      <template v-else-if="lane === 'review' || lane === 'failed'">
        <Button v-if="canCommit" variant="outline" size="xs" :disabled="busy" @click="emit('commit')">
          <GitCommitHorizontal class="h-3 w-3" :stroke-width="2" />{{ t('board.commitAll') }}
        </Button>
        <Button v-if="canPush" variant="outline" size="xs" :disabled="busy" @click="emit('push')">
          <Upload class="h-3 w-3" :stroke-width="2" />{{ t('board.pushPr') }}
        </Button>
        <Button variant="primary" size="xs" :disabled="busy" :title="t('board.finishTitle')" @click="emit('finish')">
          <Check class="h-3 w-3" :stroke-width="2" />{{ t('board.finish') }}
        </Button>
        <Button
          variant="destructive-ghost"
          size="icon-sm"
          :disabled="busy"
          :title="t('board.discardTitle')"
          :aria-label="t('board.discardTitle')"
          @click="emit('discard')"
        >
          <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </template>
    </div>
  </Card>
</template>
