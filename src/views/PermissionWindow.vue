<script setup lang="ts">
// Bare, chrome-less host for one run's pop-out permission prompt, shown in a
// standalone OS window (opened via openPermissionWindow / `?permissionWindow=1&runId=…`).
//
// This window is a REMOTE VIEW only: the main window (PermissionWindowManager)
// remains the single source of truth for the permission queue. We render whatever
// it syncs to us and forward the user's decision back — it runs the actual resolve
// logic (shiftPermission + respond_permission), so there is exactly one writer and
// no double-handling. All events are namespaced by runId so each run's window only
// ever sees its own run.
import { onMounted, onBeforeUnmount, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Pin, PinOff, Inbox } from 'lucide-vue-next'
// The main window owns the popped-out state and listens to this window's
// 'tauri://destroyed' lifecycle event to re-dock, so this window doesn't need
// to announce its own close.
import PermissionPrompt, { type PermissionRequest } from '@/components/PermissionPrompt.vue'
import { useMarkdown } from '@/lib/markdown'
const { t } = useI18n()

const { renderText, loadMarkdown } = useMarkdown()

// Which run this window answers for; every event is scoped to it.
const runId = new URLSearchParams(window.location.search).get('runId') ?? ''

const request = ref<PermissionRequest | null>(null)
const allowedTools = ref<string[]>([])
// Mirrors the window's always-on-top flag (opened with alwaysOnTop: true).
const pinned = ref(true)

let syncUnlisten: UnlistenFn | null = null

// Which session is asking — shown in the header and the OS title so several
// permission windows open at once can be told apart.
const session = ref<{ name: string; project: string } | null>(null)

// Main → popup: the current head request (or null when the queue is empty).
interface SyncPayload {
  request: PermissionRequest | null
  allowedTools: string[]
  session?: { name: string; project: string }
}

async function applyTitle() {
  const s = session.value
  const name = s ? (s.project && s.name !== s.project ? `${s.name} — ${s.project}` : s.name) : ''
  const title = name ? `${t('permission.window.osTitlePrefix')}: ${name}` : t('permission.window.title')
  document.title = title
  try {
    await getCurrentWindow().setTitle(title)
  } catch {
    /* window api unavailable */
  }
}

onMounted(async () => {
  document.title = t('permission.window.title')
  loadMarkdown()

  syncUnlisten = await listen<SyncPayload>(`permission:sync:${runId}`, (event) => {
    request.value = event.payload?.request ?? null
    allowedTools.value = event.payload?.allowedTools ?? []
    const next = event.payload?.session ?? null
    if (next?.name !== session.value?.name || next?.project !== session.value?.project) {
      session.value = next
      void applyTitle()
    }
  })

  // Tell the main window we're ready so it (re)sends the current state — covers
  // the case where the request arrived before this window finished mounting.
  await emit(`permission:window-ready:${runId}`)
})

onBeforeUnmount(() => {
  syncUnlisten?.()
  syncUnlisten = null
})

function onDecide(decision: 'allow' | 'deny', remember: boolean) {
  const req = request.value
  if (!req) return
  emit(`permission:decide:${runId}`, { request_id: req.request_id, decision, remember })
  // Optimistically clear so the panel doesn't linger; the main window will sync
  // the next request (or null) right after it resolves this one.
  request.value = null
}

function onAnswer(answers: Record<string, string>) {
  const req = request.value
  if (!req) return
  emit(`permission:answer:${runId}`, { request_id: req.request_id, answers })
  request.value = null
}

async function togglePin() {
  pinned.value = !pinned.value
  try {
    await getCurrentWindow().setAlwaysOnTop(pinned.value)
  } catch {
    /* window api unavailable */
  }
}
</script>

<template>
  <div class="flex h-screen w-screen flex-col bg-background text-foreground overflow-hidden">
    <!-- Slim titlebar: pin toggle so the user can drop always-on-top if it
         gets in the way while comparing against the chat on another monitor. -->
    <div class="flex items-center gap-2 px-3 h-11 border-b border-border/60 shrink-0">
      <div class="flex min-w-0 flex-col leading-tight">
        <!-- Project on top, session name below; generic label until the first sync. -->
        <span class="truncate text-[10px] uppercase tracking-wide text-foreground/50" :title="session?.project">
          {{ session?.project || t('permission.window.headerLabel') }}
        </span>
        <span v-if="session" class="truncate text-xs font-medium text-foreground/85" :title="session.name">
          {{ session.name }}
        </span>
      </div>
      <button
        type="button"
        class="ml-auto shrink-0 flex items-center gap-1 rounded px-1.5 py-1 text-[11px] text-foreground/60 hover:bg-accent/60 hover:text-foreground transition-colors cursor-pointer"
        :title="pinned ? t('permission.window.pinnedTitle') : t('permission.window.pinTitle')"
        @click="togglePin"
      >
        <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ pinned ? t('permission.window.pinned') : t('permission.window.pin') }}
      </button>
    </div>

    <!-- Pending request → the shared prompt; empty → waiting state. -->
    <div class="flex-1 min-h-0 overflow-auto">
      <PermissionPrompt
        v-if="request"
        :key="request.request_id"
        :request="request"
        :allowed-tools="allowedTools"
        :render-text="renderText"
        @decide="onDecide"
        @answer="onAnswer"
      />
      <div v-else class="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
        <Inbox class="h-8 w-8 text-foreground/20" :stroke-width="1" />
        <p class="text-xs text-foreground/40">
          {{ t('permission.window.emptyTitle') }}<br />
          {{ t('permission.window.emptyBody') }}
        </p>
      </div>
    </div>
  </div>
</template>
