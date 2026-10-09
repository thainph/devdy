<script setup lang="ts">
// A one-line inline rename field, following the explorer's rename pattern
// (FileTreeNode): it focuses + selects on mount, Enter / blur commit, Esc
// cancels, and a `finished` latch keeps Enter-then-blur from committing twice.
// IME-safe: Enter / Esc while composing (e.g. Vietnamese / Japanese input)
// belong to the IME, not to us.
//
// It only reports the outcome; the host decides what an empty or unchanged
// value means (usually: cancel without a request) and does the save.
import { nextTick, onMounted, ref } from 'vue'

const props = withDefaults(
  defineProps<{
    value: string
    placeholder?: string
    maxlength?: number
  }>(),
  { placeholder: '', maxlength: 200 },
)

const emit = defineEmits<{
  /** Trimmed value; may be empty or equal to the original. */
  commit: [value: string]
  cancel: []
}>()

const draft = ref(props.value)
const inputEl = ref<HTMLInputElement | null>(null)
let finished = false

function finish(commit: boolean) {
  if (finished) return
  finished = true
  if (commit) emit('commit', draft.value.trim())
  else emit('cancel')
}

function onKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (e.key === 'Enter') {
    e.preventDefault()
    finish(true)
  } else if (e.key === 'Escape') {
    e.preventDefault()
    e.stopPropagation()
    finish(false)
  }
}

onMounted(async () => {
  await nextTick()
  inputEl.value?.focus()
  inputEl.value?.select()
})
</script>

<template>
  <input
    ref="inputEl"
    v-model="draft"
    type="text"
    :maxlength="maxlength"
    :placeholder="placeholder"
    class="min-w-0 rounded border border-primary/60 bg-background px-1.5 py-0.5 text-foreground outline-none focus:ring-1 focus:ring-ring"
    @click.stop
    @dblclick.stop
    @keydown="onKeydown"
    @blur="finish(true)"
  >
</template>
