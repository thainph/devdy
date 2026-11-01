// Mirrors the app-wide agent state onto `<html data-agent-state>` so the aurora
// background reacts (src/assets/agent-ambient.css). Main window only — call it
// once from App.vue's setup.
//
// Cheap by construction: the snapshot reads only `status` and the length of
// `permissionQueue` per live session (never `entries`), so high-frequency stream
// events don't re-run anything. No polling — a timer exists only while a
// done/error flash is on screen.
import { computed, onScopeDispose, ref, watch } from 'vue'
import { useLiveRunsStore } from '@/stores/liveRuns'
import {
  AGENT_TRANSIENT_MS,
  detectAgentTransient,
  mergeAgentTransient,
  resolveAgentState,
  steadyAgentState,
  type AgentRunSnapshot,
  type AgentTransient,
} from '@/lib/agentState'

export function useAgentAmbient() {
  const live = useLiveRunsStore()

  const snapshots = computed<AgentRunSnapshot[]>(() => {
    const out: AgentRunSnapshot[] = []
    live.sessions.forEach((s, id) => {
      // permissionQueue holds tool permissions, AskUserQuestion and ExitPlanMode.
      out.push({ id, status: s.status, waiting: s.permissionQueue.length > 0 })
    })
    return out
  })

  const transient = ref<AgentTransient | null>(null)
  let timer: ReturnType<typeof setTimeout> | null = null

  watch(snapshots, (curr, prev) => {
    const incoming = detectAgentTransient(prev ?? [], curr)
    if (!incoming) return
    const next = mergeAgentTransient(transient.value, incoming)
    // A done arriving during an error flash leaves the error's timer alone.
    if (next !== incoming) return
    transient.value = next
    if (timer) clearTimeout(timer)
    timer = setTimeout(() => {
      transient.value = null
      timer = null
    }, AGENT_TRANSIENT_MS[next])
  })

  const state = computed(() => resolveAgentState(steadyAgentState(snapshots.value), transient.value))

  watch(
    state,
    (s) => {
      document.documentElement.dataset.agentState = s
    },
    { immediate: true },
  )

  onScopeDispose(() => {
    if (timer) clearTimeout(timer)
    delete document.documentElement.dataset.agentState
  })

  return { state }
}
