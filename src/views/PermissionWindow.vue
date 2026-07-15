<script setup lang="ts">
// Bare, chrome-less host for the pop-out permission prompt, shown in a
// standalone OS window (opened via openPermissionWindow / `?permissionWindow=1`).
//
// This window is a REMOTE VIEW only: the main window remains the single source
// of truth for the permission queue. We render whatever the main window syncs
// to us and forward the user's decision back — the main window runs the actual
// resolve logic (shiftPermission + respond_permission), so there is exactly one
// writer and no double-handling.
import { onMounted, onBeforeUnmount, ref } from 'vue'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Pin, PinOff, Inbox } from 'lucide-vue-next'
// The main window owns the popped-out state and listens to this window's
// 'tauri://destroyed' lifecycle event to re-dock, so this window doesn't need
// to announce its own close.
import PermissionPrompt, { type PermissionRequest } from '@/components/PermissionPrompt.vue'
import { useMarkdown } from '@/lib/markdown'

const { renderText, loadMarkdown } = useMarkdown()

const request = ref<PermissionRequest | null>(null)
const allowedTools = ref<string[]>([])
// Mirrors the window's always-on-top flag (opened with alwaysOnTop: true).
const pinned = ref(true)

let syncUnlisten: UnlistenFn | null = null

// Main → popup: the current head request (or null when the queue is empty).
interface SyncPayload {
  request: PermissionRequest | null
  allowedTools: string[]
}

onMounted(async () => {
  document.title = 'Permission — Devdy'
  loadMarkdown()

  syncUnlisten = await listen<SyncPayload>('permission:sync', (event) => {
    request.value = event.payload?.request ?? null
    allowedTools.value = event.payload?.allowedTools ?? []
  })

  // Tell the main window we're ready so it (re)sends the current state — covers
  // the case where the request arrived before this window finished mounting.
  await emit('permission:window-ready')
})

onBeforeUnmount(() => {
  syncUnlisten?.()
  syncUnlisten = null
})

function onDecide(decision: 'allow' | 'deny', remember: boolean) {
  const req = request.value
  if (!req) return
  emit('permission:decide', { request_id: req.request_id, decision, remember })
  // Optimistically clear so the panel doesn't linger; the main window will sync
  // the next request (or null) right after it resolves this one.
  request.value = null
}

function onAnswer(answers: Record<string, string>) {
  const req = request.value
  if (!req) return
  emit('permission:answer', { request_id: req.request_id, answers })
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
    <div class="flex items-center gap-2 px-3 h-9 border-b border-border/60 shrink-0">
      <span class="text-xs font-medium text-foreground/70">Claude — Permission</span>
      <button
        type="button"
        class="ml-auto flex items-center gap-1 rounded px-1.5 py-1 text-[11px] text-foreground/60 hover:bg-accent/60 hover:text-foreground transition-colors cursor-pointer"
        :title="pinned ? 'Đang ghim trên cùng — bấm để bỏ ghim' : 'Ghim luôn trên cùng'"
        @click="togglePin"
      >
        <component :is="pinned ? Pin : PinOff" class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ pinned ? 'Pinned' : 'Pin' }}
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
          Chưa có yêu cầu nào đang chờ.<br />
          Câu hỏi / xin quyền của run đang xem sẽ hiện ở đây.
        </p>
      </div>
    </div>
  </div>
</template>
