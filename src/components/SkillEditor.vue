<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch } from 'vue'
import { EditorView, keymap } from '@codemirror/view'
import { EditorState } from '@codemirror/state'
import { markdown } from '@codemirror/lang-markdown'
import { yaml } from '@codemirror/lang-yaml'
import { oneDark } from '@codemirror/theme-one-dark'
import { defaultKeymap } from '@codemirror/commands'
import { basicSetup } from 'codemirror'

const props = defineProps<{
  modelValue: string
  isDark?: boolean
  template?: string
}>()

const emit = defineEmits<{
  'update:modelValue': [value: string]
  save: []
}>()

const editorEl = ref<HTMLDivElement | null>(null)
let view: EditorView | null = null

function createEditor(value: string) {
  if (!editorEl.value) return

  const saveKeymap = keymap.of([
    {
      key: 'Mod-s',
      run: () => {
        emit('save')
        return true
      },
    },
  ])

  const extensions = [
    basicSetup,
    markdown({ defaultCodeLanguage: yaml() }),
    keymap.of(defaultKeymap),
    saveKeymap,
    EditorView.updateListener.of((update) => {
      if (update.docChanged) {
        emit('update:modelValue', update.state.doc.toString())
      }
    }),
    EditorView.theme({
      '&': { height: '100%', fontSize: '14px' },
      '.cm-scroller': { overflow: 'auto', fontFamily: '"JetBrains Mono", "Fira Code", monospace' },
    }),
  ]

  if (props.isDark) {
    extensions.push(oneDark)
  }

  const state = EditorState.create({ doc: value, extensions })
  view = new EditorView({ state, parent: editorEl.value })
}

onMounted(() => createEditor(props.modelValue))

onUnmounted(() => view?.destroy())

watch(() => props.modelValue, (newVal) => {
  if (view && newVal !== view.state.doc.toString()) {
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: newVal },
    })
  }
})

function insertTemplate() {
  const template = props.template ?? `---
name: skill-name
description: What this skill does
---

# Skill Name

Describe what this skill does and how to use it.
`
  if (view) {
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: template },
    })
    emit('update:modelValue', template)
  }
}

defineExpose({ insertTemplate })
</script>

<template>
  <div ref="editorEl" class="h-full w-full overflow-hidden" />
</template>
