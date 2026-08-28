import { listen } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'

// Run a translation and resolve with the FULL translated text.
//
// The backend `translate_text` command reuses a warm sidecar and streams the
// answer: it returns a turn id immediately and emits `translate:done` /
// `translate:error` events tagged with that id. This helper wraps that streaming
// protocol back into a simple `Promise<string>` for callers that only want the
// final text (e.g. calendar batch translation). Components that want to render
// the translation incrementally listen to `translate:chunk` directly instead.
export async function translateText(text: string, targetLang?: string): Promise<string> {
  let turn = -1
  let settle!: (r: { ok: true; text: string } | { ok: false; error: string }) => void
  const done = new Promise<{ ok: true; text: string } | { ok: false; error: string }>((resolve) => {
    settle = resolve
  })

  // Subscribe before invoking so no completion event is missed.
  const unDone = await listen<{ turn: number; text: string }>('translate:done', (e) => {
    if (e.payload.turn === turn) settle({ ok: true, text: e.payload.text })
  })
  const unError = await listen<{ turn: number; error: string }>('translate:error', (e) => {
    if (e.payload.turn === turn) settle({ ok: false, error: e.payload.error })
  })

  try {
    turn = await invoke<number>('translate_text', { text, targetLang })
  } catch (e) {
    unDone()
    unError()
    throw e
  }

  const res = await done
  unDone()
  unError()
  if (!res.ok) throw new Error(res.error)
  return res.text
}
