<script setup lang="ts">
// Right pane of the Rules screen: one rule (or a new draft when `ruleId` is
// null) edited in place, laid out like SkillDetail. The markdown file is the
// source of truth — the name, description, target and Codex mode fields just
// rewrite its frontmatter lines.
//
// The host keys this component by the selection, so every rule starts from a
// fresh draft; it asks `dirty` before switching. List-level actions (reapply,
// apply to all, export, delete) are emitted for the host to confirm and run.
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertCircle, Columns2, Download, Eye, FileText, FolderCheck, FolderOpen, LayoutTemplate,
  MoreHorizontal, Pencil, RefreshCw, Trash2, X,
} from 'lucide-vue-next'
import { AppSelect, Button, DropdownItem, DropdownMenu, DropdownSeparator, Input } from '@/components/ui'
import SkillEditor from '@/components/SkillEditor.vue'
import DetailHeader from '@/components/listDetail/DetailHeader.vue'
import DetailSection from '@/components/listDetail/DetailSection.vue'
import { useRulesStore, type Rule, type RuleTarget } from '@/stores/rules'
import type { Group } from '@/stores/groups'
import { useToast } from '@/composables/useToast'
import { applyMermaidFence, vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import { relativeAge } from '@/lib/itemText'
import { formatFullDate } from '@/lib/slackThreadUtils'
import { parseFrontmatter, setFrontmatterField, validateFrontmatter, type CodexMode } from '@/lib/rulesRows'

const props = defineProps<{
  /** null = a new, unsaved rule. */
  ruleId: string | null
  /** The list entry of the rule (meta line, external-change detection). */
  rule: Rule | null
  /** Groups the rule belongs to. */
  groups: Group[]
  /** This rule is being re-applied (spinner). */
  reapplying: boolean
  /** Some re-apply is running (all buttons wait). */
  reapplyBusy: boolean
  applying: boolean
  deleting: boolean
}>()

const emit = defineEmits<{
  saved: [rule: Rule, created: boolean]
  reapply: []
  applyAll: []
  export: []
  delete: []
  discard: []
  /** The rule could not be loaded (deleted, stale id): the host drops the selection. */
  missing: []
}>()

const { t } = useI18n()
const store = useRulesStore()
const { toast } = useToast()

const RULE_TEMPLATE = `---
name: my-rule
description: What this convention enforces
target: both
---

## My Rule

- Describe the convention here.
`

const isNew = computed(() => props.ruleId === null)
const content = ref(RULE_TEMPLATE)
/** What was last loaded or saved; the draft is dirty when it differs. */
const original = ref(RULE_TEMPLATE)
const loading = ref(false)
/** Loading failed: the draft is still the template, so it must never be saved over the rule. */
const loadFailed = ref(false)
const saving = ref(false)
const editorRef = ref<InstanceType<typeof SkillEditor> | null>(null)

const fm = computed(() => parseFrontmatter(content.value))
const target = computed<RuleTarget>(() => (fm.value.target || 'both') as RuleTarget)
const codexMode = computed<CodexMode>(() => (fm.value.codexMode === 'skill' ? 'skill' : 'inline'))
const validationError = computed(() => {
  const err = validateFrontmatter(content.value)
  return err ? t(`rules.editor.${err}`) : null
})
const dirty = computed(() => content.value !== original.value)
const canSave = computed(
  () => dirty.value && !saving.value && !loading.value && !loadFailed.value && !validationError.value,
)

const targetLabels = computed<Record<RuleTarget, string>>(() => ({
  both: t('rules.editor.targetBoth'),
  claude: t('rules.editor.targetClaude'),
  codex: t('rules.editor.targetCodex'),
}))

const meta = computed(() => {
  const parts = [t('rules.detail.metaTarget', { target: targetLabels.value[target.value] })]
  if (target.value !== 'claude') {
    parts.push(t('rules.detail.metaCodex', {
      mode: codexMode.value === 'skill' ? t('rules.editor.codexModeSkill') : t('rules.editor.codexModeInline'),
    }))
  }
  if (props.groups.length > 0) {
    parts.push(t('rules.detail.metaGroups', { names: props.groups.map((g) => g.name).join(', ') }))
  }
  parts.push(props.rule ? t('rules.detail.metaUpdated', { age: relativeAge(props.rule.updated_at) }) : t('rules.detail.metaDraft'))
  return parts.join(' · ')
})

const metaTitle = computed(() =>
  props.rule ? `${t('rules.detail.metaUpdated', { age: formatFullDate(props.rule.updated_at) })}\n${props.rule.source_path}` : '',
)

async function load() {
  if (!props.ruleId) return
  loading.value = true
  try {
    const rc = await store.getRule(props.ruleId)
    content.value = rc.content
    original.value = rc.content
    loadFailed.value = false
  } catch (e) {
    loadFailed.value = true
    toast.error(String(e))
    emit('missing')
  } finally {
    loading.value = false
  }
}

onMounted(load)

// Changed elsewhere (MCP tools, another window) while untouched here: show it.
watch(
  () => props.rule?.updated_at,
  (now, before) => {
    if (now && before && now !== before && !dirty.value && !saving.value) void load()
  },
)

// ── Preview ─────────────────────────────────────────────────────────────────

type ViewMode = 'edit' | 'split' | 'preview'
const VIEW_KEY = 'devdy.rules.view'
const viewMode = ref<ViewMode>(readViewMode())
const viewModes = computed(() => [
  { value: 'edit' as const, label: t('rules.detail.markdown'), icon: Pencil },
  { value: 'split' as const, label: t('rules.detail.split'), icon: Columns2 },
  { value: 'preview' as const, label: t('rules.detail.preview'), icon: Eye },
])

function readViewMode(): ViewMode {
  try {
    const v = localStorage.getItem(VIEW_KEY)
    return v === 'edit' || v === 'preview' ? v : 'split'
  } catch {
    return 'split'
  }
}

function setViewMode(v: ViewMode) {
  viewMode.value = v
  try {
    localStorage.setItem(VIEW_KEY, v)
  } catch {
    /* storage unavailable */
  }
}

const previewHtml = ref('')
let previewSeq = 0
async function updatePreview(md: string) {
  const seq = ++previewSeq
  let html: string
  try {
    const MarkdownIt = (await import('markdown-it')).default
    const mdi = new MarkdownIt({ html: false, linkify: true, typographer: true })
    applyMermaidFence(mdi)
    html = mdi.render(md)
  } catch {
    html = `<pre>${md.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c] ?? c)}</pre>`
  }
  if (seq === previewSeq) previewHtml.value = html
}

// Only render while a preview is on screen.
watch([content, viewMode], ([md, mode]) => {
  if (mode !== 'edit') void updatePreview(md)
}, { immediate: true })

// ── Metadata fields ─────────────────────────────────────────────────────────
// Local drafts: the frontmatter stores trimmed values, so binding the inputs to
// the parsed values directly would eat a space while it is being typed.
const nameDraft = ref('')
const descriptionDraft = ref('')

watch(fm, (v) => {
  if (v.name !== nameDraft.value.trim()) nameDraft.value = v.name
  if (v.description !== descriptionDraft.value.trim()) descriptionDraft.value = v.description
}, { immediate: true })

function setText(key: 'name' | 'description', value: string) {
  if (key === 'name') nameDraft.value = value
  else descriptionDraft.value = value
  setField(key, value.trim())
}

/** The rule file's name, for the file tab. */
const fileName = computed(() => {
  const path = props.rule?.source_path
  if (path) return path.slice(Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\')) + 1)
  return `${fm.value.name || 'rule'}.md`
})

function setField(key: string, value: string | null) {
  content.value = setFrontmatterField(content.value, key, value)
}

// Inline is the default, so it is expressed by leaving `codex_mode:` out.
function setCodexMode(mode: CodexMode) {
  setField('codex_mode', mode === 'skill' ? 'skill' : null)
}

async function save() {
  if (!canSave.value) return
  const f = parseFrontmatter(content.value)
  const payload = { name: f.name, description: f.description, target: (f.target || 'both') as RuleTarget, content: content.value }
  saving.value = true
  try {
    const saved = props.ruleId
      ? await store.updateRule({ id: props.ruleId, ...payload })
      : await store.createRule(payload)
    original.value = payload.content
    toast.success(t('rules.editor.toastSaved'))
    emit('saved', saved, !props.ruleId)
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

function onKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') {
    e.preventDefault()
    void save()
  }
}

async function openRulesFolder() {
  try {
    await store.openRulesFolder()
  } catch (e) {
    toast.error(String(e))
  }
}

defineExpose({ dirty, save })
</script>

<template>
  <section class="flex min-w-0 flex-1 flex-col" @keydown="onKeydown">
    <DetailHeader :title="fm.name" :placeholder="t('rules.editor.newRule')">
      <template #meta>
        <span :title="metaTitle">{{ meta }}</span>
      </template>
      <template #actions>
        <Button size="xs" :disabled="!canSave" :title="t('rules.detail.saveHint')" @click="save">
          {{ saving ? t('rules.editor.saving') : t('common.save') }}
        </Button>
        <DropdownMenu align="right">
          <template #trigger>
            <Button variant="outline" size="icon-sm" :title="t('rules.detail.moreActions')" :aria-label="t('rules.detail.moreActions')">
              <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
          <DropdownItem :disabled="loading" @click="editorRef?.insertTemplate()">
            <LayoutTemplate class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('rules.editor.insertTemplate') }}
          </DropdownItem>
          <DropdownItem @click="openRulesFolder">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('rules.editor.openFolder') }}
          </DropdownItem>
          <template v-if="!isNew">
            <DropdownSeparator />
            <DropdownItem :disabled="applying" @click="emit('applyAll')">
              <FolderCheck class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('rules.list.applyToAll') }}
            </DropdownItem>
            <DropdownItem :disabled="reapplyBusy" @click="emit('reapply')">
              <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" :class="{ 'animate-spin': reapplying }" />
              {{ t('rules.list.reapply') }}
            </DropdownItem>
            <DropdownItem @click="emit('export')">
              <Download class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('rules.list.exportMd') }}
            </DropdownItem>
          </template>
          <DropdownSeparator />
          <DropdownItem v-if="isNew" variant="destructive" @click="emit('discard')">
            <X class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('rules.detail.discardDraft') }}
          </DropdownItem>
          <DropdownItem v-else variant="destructive" :disabled="deleting" @click="emit('delete')">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('common.delete') }}
          </DropdownItem>
        </DropdownMenu>
      </template>
    </DetailHeader>

    <div v-if="loading || loadFailed" class="flex flex-1 items-center justify-center text-sm text-muted-foreground">
      {{ t('common.loading') }}
    </div>

    <template v-else>
      <div class="shrink-0 px-5">
        <DetailSection :title="t('rules.detail.details')" :hint="t('rules.detail.detailsHint')">
          <div
            class="grid gap-3"
            :class="target !== 'claude' ? 'grid-cols-[minmax(0,1fr)_8rem_8rem]' : 'grid-cols-[minmax(0,1fr)_8rem]'"
          >
            <label class="block space-y-1">
              <span class="text-[11px] font-medium text-muted-foreground">{{ t('rules.detail.name') }}</span>
              <Input
                :model-value="nameDraft"
                class="font-mono"
                placeholder="my-rule"
                @update:model-value="(v: string) => setText('name', v)"
              />
            </label>
            <label class="block space-y-1">
              <span class="text-[11px] font-medium text-muted-foreground">{{ t('rules.editor.target') }}</span>
              <AppSelect
                :model-value="target"
                :options="[
                  { value: 'both', label: t('rules.editor.targetBoth') },
                  { value: 'claude', label: t('rules.editor.targetClaude') },
                  { value: 'codex', label: t('rules.editor.targetCodex') },
                ]"
                @update:model-value="(v: string) => setField('target', v)"
              />
            </label>
            <label v-if="target !== 'claude'" class="block space-y-1" :title="t('rules.editor.codexModeHint')">
              <span class="text-[11px] font-medium text-muted-foreground">{{ t('rules.editor.codexMode') }}</span>
              <AppSelect
                :model-value="codexMode"
                :options="[
                  { value: 'inline', label: t('rules.editor.codexModeInline') },
                  { value: 'skill', label: t('rules.editor.codexModeSkill') },
                ]"
                @update:model-value="(v: string) => setCodexMode(v as CodexMode)"
              />
            </label>
            <label class="block space-y-1" :class="target !== 'claude' ? 'col-span-3' : 'col-span-2'">
              <span class="text-[11px] font-medium text-muted-foreground">{{ t('rules.detail.description') }}</span>
              <Input
                :model-value="descriptionDraft"
                :placeholder="t('rules.detail.descriptionPlaceholder')"
                @update:model-value="(v: string) => setText('description', v)"
              />
            </label>
          </div>
          <p v-if="target !== 'claude'" class="text-[11px] text-muted-foreground/80">
            {{ t('rules.editor.codexModeHint') }}
          </p>
        </DetailSection>
      </div>

      <!-- File tab + view mode -->
      <div class="flex shrink-0 items-center gap-2 border-y border-border/60 bg-muted/30 px-3 py-1">
        <div class="flex min-w-0 flex-1 items-center gap-0.5">
          <span
            class="flex shrink-0 items-center gap-1 rounded bg-background px-2 py-1 font-mono text-[11px] text-foreground shadow-sm"
            :title="rule?.source_path ?? fileName"
          >
            <FileText class="h-3 w-3 shrink-0" :stroke-width="1.75" />
            <span class="max-w-48 truncate">{{ fileName }}</span>
            <span v-if="dirty" class="h-1.5 w-1.5 rounded-full bg-primary" />
          </span>
        </div>
        <div class="flex shrink-0 rounded-md bg-muted p-0.5" role="group">
          <button
            v-for="m in viewModes"
            :key="m.value"
            type="button"
            class="flex cursor-pointer items-center rounded p-1 transition-colors"
            :class="viewMode === m.value
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground'"
            :title="m.label"
            :aria-label="m.label"
            :aria-pressed="viewMode === m.value"
            @click="setViewMode(m.value)"
          >
            <component :is="m.icon" class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
        </div>
      </div>

      <div
        v-if="validationError"
        class="flex shrink-0 items-center gap-1.5 border-b border-amber-500/20 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-500"
      >
        <AlertCircle class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        <span class="min-w-0 truncate" :title="validationError">{{ validationError }}</span>
      </div>

      <div class="flex min-h-0 flex-1">
        <div
          v-show="viewMode !== 'preview'"
          class="flex min-w-0 flex-1 flex-col overflow-hidden"
          :class="viewMode === 'split' && 'border-r border-border/60'"
        >
          <SkillEditor
            ref="editorRef"
            v-model="content"
            :is-dark="true"
            :template="RULE_TEMPLATE"
            class="flex-1"
          />
        </div>
        <div v-if="viewMode !== 'edit'" class="min-w-0 flex-1 overflow-auto p-5">
          <div
            v-mermaid
            v-copy-code
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
            v-html="previewHtml"
          />
        </div>
      </div>
    </template>
  </section>
</template>
