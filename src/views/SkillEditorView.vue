<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useSkillsStore, type SkillTarget } from '@/stores/skills'
import SkillEditor from '@/components/SkillEditor.vue'
import { Button, AppSelect } from '@/components/ui'
import { invoke } from '@/lib/tauri'
import { ArrowLeft, FileCode2, Save, LayoutTemplate, AlertCircle, FolderOpen } from 'lucide-vue-next'

const route = useRoute()
const router = useRouter()
const store = useSkillsStore()

const isNew = computed(() => route.name === 'skill-new')
const skillId = computed(() => route.params.id as string | undefined)

const SKILL_TEMPLATE = `---
name: my-skill
description: What this skill does
target: both
---

# My Skill

Describe what this skill does.
`

const content = ref(SKILL_TEMPLATE)
const name = ref('')
const description = ref('')
const target = ref<SkillTarget>('both')
const saving = ref(false)
const loading = ref(false)
const validationError = ref<string | null>(null)
const editorRef = ref<InstanceType<typeof SkillEditor> | null>(null)
const previewHtml = ref('')
const sourcePath = ref<string | null>(null)

function parseFrontmatter(raw: string) {
  if (!raw.startsWith('---')) return { name: '', description: '', target: '' }
  const rest = raw.slice(3)
  const end = rest.indexOf('---')
  if (end === -1) return { name: '', description: '', target: '' }
  const fm = rest.slice(0, end)
  const pick = (key: string) =>
    fm.match(new RegExp(`^${key}:\\s*(.+)$`, 'm'))?.[1]?.trim().replace(/^["']|["']$/g, '') ?? ''
  return { name: pick('name'), description: pick('description'), target: pick('target') }
}

// A plain (unquoted) YAML scalar must not start with an indicator char, contain
// `: ` / ` #`, or end with a colon — otherwise it needs double-quoting.
function isPlainSafe(v: string): boolean {
  if (v === '' || /^[\s!&*?|>%@`"'#,[\]{}:-]/.test(v)) return false
  if (/:(\s|$)/.test(v) || /\s#/.test(v) || /[\n\t]/.test(v)) return false
  return true
}

function yamlQuote(v: string): string {
  return '"' + v.replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"'
}

// Collapse any frontmatter value that was wrapped onto multiple unquoted lines
// back into a single valid line. Returns the (possibly rewritten) content plus a
// hard error when a line can't be interpreted (block save in that case).
function normalizeFrontmatter(raw: string): { content: string; changed: boolean; error: string | null } {
  const lines = raw.split('\n')
  if (lines[0]?.trim() !== '---') return { content: raw, changed: false, error: null }
  let end = -1
  for (let i = 1; i < lines.length; i++) {
    if (lines[i].trim() === '---') { end = i; break }
  }
  if (end === -1) return { content: raw, changed: false, error: 'Frontmatter is missing its closing `---`' }

  const keyRe = /^([A-Za-z0-9_-]+):[ \t]?(.*)$/
  const out: string[] = []
  let changed = false
  let i = 1
  while (i < end) {
    const line = lines[i]
    if (line.trim() === '' || line.trimStart().startsWith('#')) { out.push(line); i++; continue }
    const km = line.match(keyRe)
    if (!km) return { content: raw, changed: false, error: `Invalid frontmatter line (expected \`key: value\`): ${line.trim()}` }
    const key = km[1]
    let value = km[2]
    // Block scalars (| or >) are already valid YAML — copy them and their indented body verbatim.
    if (/^[|>]/.test(value.trim())) {
      out.push(line); i++
      while (i < end && (lines[i].trim() === '' || /^\s/.test(lines[i]))) { out.push(lines[i]); i++ }
      continue
    }
    // Gather continuation lines: non-blank lines that don't start a new key.
    let j = i + 1
    const cont: string[] = []
    while (j < end && lines[j].trim() !== '' && !keyRe.test(lines[j])) { cont.push(lines[j].trim()); j++ }
    if (cont.length) {
      value = [value.trim(), ...cont].join(' ').trim()
      out.push(`${key}: ${isPlainSafe(value) ? value : yamlQuote(value)}`)
      changed = true
      i = j
    } else {
      out.push(line); i++
    }
  }

  if (!changed) return { content: raw, changed: false, error: null }
  const rebuilt = ['---', ...out, '---', ...lines.slice(end + 1)].join('\n')
  return { content: rebuilt, changed: true, error: null }
}

function validateFrontmatter(raw: string): string | null {
  const norm = normalizeFrontmatter(raw)
  if (norm.error) return norm.error
  const { name: n, description: d, target: t } = parseFrontmatter(norm.content)
  if (!n) return 'Frontmatter missing required field: name'
  if (!/^[a-zA-Z0-9_-]+$/.test(n)) return 'name must be a slug (letters, numbers, hyphens, underscores)'
  if (!d) return 'Frontmatter missing required field: description'
  if (t && !['claude', 'codex', 'both'].includes(t)) return 'target must be one of: claude, codex, both'
  return null
}

// Target select rewrites the frontmatter `target:` line (content stays source of truth).
function setTarget(t: SkillTarget) {
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

async function updatePreview(md: string) {
  try {
    const MarkdownIt = (await import('markdown-it')).default
    const mdi = new MarkdownIt({ html: false, linkify: true, typographer: true })
    previewHtml.value = mdi.render(md)
  } catch {
    previewHtml.value = `<pre>${md}</pre>`
  }
}

watch(content, (val) => {
  const fm = parseFrontmatter(val)
  name.value = fm.name
  description.value = fm.description
  target.value = (fm.target || 'both') as SkillTarget
  validationError.value = validateFrontmatter(val)
  updatePreview(val)
}, { immediate: true })

onMounted(async () => {
  if (!isNew.value && skillId.value) {
    loading.value = true
    try {
      const sc = await store.getSkill(skillId.value)
      content.value = sc.content
      sourcePath.value = sc.skill.source_path
    } catch (e) {
      alert(String(e))
      router.push('/skills')
    } finally {
      loading.value = false
    }
  }
})

async function openSkillFolder() {
  if (skillId.value) await invoke('open_skill_folder', { id: skillId.value })
}

async function handleSave() {
  // Auto-normalize multi-line frontmatter values before validating/saving so the
  // written SKILL.md is always valid YAML; block save only if it can't be fixed.
  const norm = normalizeFrontmatter(content.value)
  if (norm.error) { validationError.value = norm.error; return }
  if (norm.changed) content.value = norm.content

  const err = validateFrontmatter(content.value)
  if (err) { validationError.value = err; return }

  const fm = parseFrontmatter(content.value)
  const t = (fm.target || 'both') as SkillTarget
  saving.value = true
  try {
    if (isNew.value) {
      await store.createSkill({ name: fm.name, description: fm.description, target: t, content: content.value })
    } else {
      await store.updateSkill({ id: skillId.value!, name: fm.name, description: fm.description, target: t, content: content.value })
    }
    router.push('/skills')
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
        title="Back to Skills"
        @click="router.push('/skills')"
      >
        <ArrowLeft class="h-4 w-4" :stroke-width="1.75" />
      </Button>

      <div class="w-px h-4 bg-border/60 mx-1" />

      <div class="flex items-center gap-1.5 text-sm">
        <FileCode2 class="h-4 w-4 text-muted-foreground" :stroke-width="1.5" />
        <span class="font-medium">{{ isNew ? 'New Skill' : (name || '…') }}</span>
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
          @update:model-value="(v: string) => setTarget(v as SkillTarget)"
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
        v-if="!isNew && sourcePath"
        variant="outline"
        title="Open skill folder in Finder"
        @click="openSkillFolder"
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
          :template="SKILL_TEMPLATE"
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
