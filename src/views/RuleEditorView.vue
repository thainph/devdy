<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useRulesStore, type RuleTarget } from '@/stores/rules'
import SkillEditor from '@/components/SkillEditor.vue'
import { Button, AppSelect } from '@/components/ui'
import { applyMermaidFence, vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import { ArrowLeft, ScrollText, Save, LayoutTemplate, AlertCircle, FolderOpen } from 'lucide-vue-next'

const route = useRoute()
const router = useRouter()
const store = useRulesStore()

const isNew = computed(() => route.name === 'rule-new')
const ruleId = computed(() => route.params.id as string | undefined)

const RULE_TEMPLATE = `---
name: my-rule
description: What this convention enforces
target: both
---

## My Rule

- Describe the convention here.
`

const content = ref(RULE_TEMPLATE)
const name = ref('')
const description = ref('')
const target = ref<RuleTarget>('both')
const saving = ref(false)
const loading = ref(false)
const validationError = ref<string | null>(null)
const editorRef = ref<InstanceType<typeof SkillEditor> | null>(null)
const previewHtml = ref('')

function parseFrontmatter(raw: string) {
  if (!raw.startsWith('---')) return { name: '', description: '', target: '' }
  const rest = raw.slice(3)
  const end = rest.indexOf('\n---')
  if (end === -1) return { name: '', description: '', target: '' }
  const fm = rest.slice(0, end)
  const pick = (key: string) =>
    fm.match(new RegExp(`^${key}:\\s*(.+)$`, 'm'))?.[1]?.trim().replace(/^["']|["']$/g, '') ?? ''
  return { name: pick('name'), description: pick('description'), target: pick('target') }
}

function validateFrontmatter(raw: string): string | null {
  const { name: n, description: d, target: t } = parseFrontmatter(raw)
  if (!n) return 'Frontmatter missing required field: name'
  if (!/^[a-zA-Z0-9_-]+$/.test(n)) return 'name must be a slug (letters, numbers, hyphens, underscores)'
  if (!d) return 'Frontmatter missing required field: description'
  if (t && !['claude', 'codex', 'both'].includes(t)) return 'target must be one of: claude, codex, both'
  return null
}

async function updatePreview(md: string) {
  try {
    const MarkdownIt = (await import('markdown-it')).default
    const mdi = new MarkdownIt({ html: false, linkify: true, typographer: true })
    applyMermaidFence(mdi)
    previewHtml.value = mdi.render(md)
  } catch {
    previewHtml.value = `<pre>${md}</pre>`
  }
}

watch(content, (val) => {
  const fm = parseFrontmatter(val)
  name.value = fm.name
  description.value = fm.description
  target.value = (fm.target || 'both') as RuleTarget
  validationError.value = validateFrontmatter(val)
  updatePreview(val)
}, { immediate: true })

// Target select rewrites the frontmatter `target:` line (content stays source of truth).
function setTarget(t: RuleTarget) {
  const raw = content.value
  if (!raw.startsWith('---')) return
  const rest = raw.slice(3)
  const end = rest.indexOf('\n---')
  if (end === -1) return
  let fm = rest.slice(0, end)
  const after = rest.slice(end)
  if (/^target:.*$/m.test(fm)) {
    fm = fm.replace(/^target:.*$/m, `target: ${t}`)
  } else {
    fm = fm.replace(/\n*$/, '') + `\ntarget: ${t}\n`
  }
  content.value = '---' + fm + after
}

onMounted(async () => {
  if (!isNew.value && ruleId.value) {
    loading.value = true
    try {
      const rc = await store.getRule(ruleId.value)
      content.value = rc.content
    } catch (e) {
      alert(String(e))
      router.push('/rules')
    } finally {
      loading.value = false
    }
  }
})

async function openRulesFolder() {
  await store.openRulesFolder()
}

async function handleSave() {
  const err = validateFrontmatter(content.value)
  if (err) { validationError.value = err; return }

  const fm = parseFrontmatter(content.value)
  const t = (fm.target || 'both') as RuleTarget
  saving.value = true
  try {
    if (isNew.value) {
      await store.createRule({ name: fm.name, description: fm.description, target: t, content: content.value })
    } else {
      await store.updateRule({ id: ruleId.value!, name: fm.name, description: fm.description, target: t, content: content.value })
    }
    router.push('/rules')
  } catch (e) {
    alert(String(e))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Toolbar -->
    <div class="flex items-center gap-2 px-4 h-13 border-b border-border/60 bg-card/40 shrink-0">
      <Button
        variant="ghost"
        size="icon-sm"
        title="Back to Rules"
        @click="router.push('/rules')"
      >
        <ArrowLeft class="h-4 w-4" :stroke-width="1.75" />
      </Button>

      <div class="w-px h-4 bg-border/60 mx-1" />

      <div class="flex items-center gap-1.5 text-sm">
        <ScrollText class="h-4 w-4 text-muted-foreground" :stroke-width="1.5" />
        <span class="font-medium">{{ isNew ? 'New Rule' : (name || '…') }}</span>
        <span v-if="!isNew" class="text-muted-foreground font-normal">— editing</span>
      </div>

      <div class="w-px h-4 bg-border/60 mx-1" />

      <div class="flex items-center gap-1.5">
        <span class="text-[10px] font-medium text-muted-foreground uppercase tracking-wider">Target</span>
        <AppSelect
          :model-value="target"
          size="sm"
          class="w-28"
          :options="[
            { value: 'both', label: 'Both' },
            { value: 'claude', label: 'Claude' },
            { value: 'codex', label: 'Codex' },
          ]"
          @update:model-value="(v: string) => setTarget(v as RuleTarget)"
        />
      </div>

      <div class="flex-1" />

      <Button
        variant="outline"
        @click="editorRef?.insertTemplate()"
        title="Insert frontmatter template"
      >
        <LayoutTemplate class="h-3.5 w-3.5" :stroke-width="1.75" />
        Template
      </Button>

      <Button
        variant="outline"
        title="Open rules folder in Finder"
        @click="openRulesFolder"
      >
        <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
        Open in Finder
      </Button>

      <!-- Validation error inline -->
      <div
        v-if="validationError"
        class="flex items-center gap-1 px-2.5 py-1 text-xs text-amber-500 bg-amber-500/10 border border-amber-500/20 rounded-md"
      >
        <AlertCircle class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        <span class="max-w-48 truncate">{{ validationError }}</span>
      </div>

      <Button
        :disabled="saving || !!validationError"
        @click="handleSave"
        title="Save (⌘S)"
      >
        <Save class="h-3.5 w-3.5" :stroke-width="2" />
        {{ saving ? 'Saving…' : 'Save' }}
      </Button>
    </div>

    <!-- Loading -->
    <div v-if="loading" class="flex-1 flex items-center justify-center text-muted-foreground text-sm">
      Loading…
    </div>

    <!-- Split view -->
    <div v-else class="flex-1 flex overflow-hidden">
      <!-- Editor pane -->
      <div class="flex-1 border-r border-border/60 overflow-hidden flex flex-col">
        <div class="flex items-center gap-1.5 px-3 py-1.5 border-b border-border/40 bg-muted/30">
          <span class="text-[10px] font-medium text-muted-foreground uppercase tracking-wider">Markdown</span>
        </div>
        <SkillEditor
          ref="editorRef"
          v-model="content"
          :is-dark="true"
          :template="RULE_TEMPLATE"
          class="flex-1"
          @save="handleSave"
        />
      </div>

      <!-- Preview pane -->
      <div class="flex-1 overflow-auto flex flex-col">
        <div class="flex items-center gap-1.5 px-3 py-1.5 border-b border-border/40 bg-muted/30 shrink-0">
          <span class="text-[10px] font-medium text-muted-foreground uppercase tracking-wider">Preview</span>
        </div>
        <div class="flex-1 overflow-auto p-5">
          <div
            v-mermaid
            v-copy-code
            v-html="previewHtml"
            class="text-sm leading-relaxed
              [&_h1]:text-lg [&_h1]:font-bold [&_h1]:mb-3 [&_h1]:mt-0
              [&_h2]:text-base [&_h2]:font-semibold [&_h2]:mb-2 [&_h2]:mt-4
              [&_h3]:text-sm [&_h3]:font-semibold [&_h3]:mb-1.5 [&_h3]:mt-3
              [&_p]:mb-3 [&_p]:text-foreground/90
              [&_ul]:list-disc [&_ul]:pl-5 [&_ul]:mb-3
              [&_ol]:list-decimal [&_ol]:pl-5 [&_ol]:mb-3
              [&_li]:mb-1
              [&_code]:font-mono [&_code]:bg-muted [&_code]:px-1 [&_code]:rounded [&_code]:text-xs [&_code]:text-foreground/80
              [&_pre]:bg-muted [&_pre]:p-3 [&_pre]:rounded-md [&_pre]:overflow-auto [&_pre]:mb-3 [&_pre]:border [&_pre]:border-border/50
              [&_blockquote]:border-l-2 [&_blockquote]:border-primary/40 [&_blockquote]:pl-4 [&_blockquote]:text-muted-foreground [&_blockquote]:mb-3
              [&_hr]:border-border/60 [&_hr]:my-4
              [&_a]:text-primary [&_a]:underline [&_a]:underline-offset-2
              [&_strong]:font-semibold"
          />
        </div>
      </div>
    </div>
  </div>
</template>
