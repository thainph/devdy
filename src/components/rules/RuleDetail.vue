<script setup lang="ts">
// Right pane of the Rules screen: one rule (or a new draft when `ruleId` is
// null) edited in place. The markdown file is the source of truth — the name,
// target and Codex mode controls just rewrite its frontmatter lines.
//
// The host keys this component by the selection, so every rule starts from a
// fresh draft; it asks `dirty` before switching. List-level actions (reapply,
// apply to all, export, delete) are emitted for the host to confirm and run.
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertCircle, Download, Eye, EyeOff, FolderCheck, FolderOpen, LayoutTemplate,
  MoreHorizontal, RefreshCw, ScrollText, Trash2, X,
} from 'lucide-vue-next'
import { AppSelect, Button, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'
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
const showPreview = ref(true)
const previewHtml = ref('')
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

async function updatePreview(md: string) {
  try {
    const MarkdownIt = (await import('markdown-it')).default
    const mdi = new MarkdownIt({ html: false, linkify: true, typographer: true })
    applyMermaidFence(mdi)
    previewHtml.value = mdi.render(md)
  } catch {
    previewHtml.value = `<pre>${md.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c] ?? c)}</pre>`
  }
}

watch(content, (val) => { if (showPreview.value) void updatePreview(val) }, { immediate: true })
watch(showPreview, (on) => { if (on) void updatePreview(content.value) })

function setField(key: string, value: string | null) {
  content.value = setFrontmatterField(content.value, key, value)
}

function rename(value: string) {
  setField('name', value)
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
    <DetailHeader
      :title="fm.name"
      :placeholder="t('rules.editor.newRule')"
      renamable
      :rename-hint="t('rules.detail.renameHint')"
      @rename="rename"
    >
      <template #leading>
        <ScrollText class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
      </template>
      <template #meta>
        <span :title="metaTitle">{{ meta }}</span>
      </template>
      <template #actions>
        <div
          v-if="validationError && !loading"
          class="flex min-w-0 items-center gap-1 rounded-md border border-amber-500/20 bg-amber-500/10 px-2 py-1 text-[11px] text-amber-500"
          :title="validationError"
        >
          <AlertCircle class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span class="max-w-48 truncate">{{ validationError }}</span>
        </div>
        <Button
          variant="primary"
          size="xs"
          class="shrink-0"
          :disabled="!canSave"
          :title="t('rules.detail.saveHint')"
          @click="save"
        >
          {{ saving ? t('rules.editor.saving') : t('common.save') }}
        </Button>
        <Button
          v-if="!isNew"
          variant="outline"
          size="icon-sm"
          class="shrink-0"
          :title="t('rules.list.reapply')"
          :aria-label="t('rules.list.reapply')"
          :disabled="reapplyBusy"
          @click="emit('reapply')"
        >
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" :class="{ 'animate-spin': reapplying }" />
        </Button>
        <DropdownMenu align="right" class="shrink-0">
          <template #trigger>
            <Button
              variant="outline"
              size="icon-sm"
              :title="t('rules.detail.moreActions')"
              :aria-label="t('rules.detail.moreActions')"
            >
              <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
          <template v-if="!isNew">
            <DropdownItem :disabled="applying" @click="emit('applyAll')">
              <FolderCheck class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('rules.list.applyToAll') }}
            </DropdownItem>
            <DropdownItem @click="emit('export')">
              <Download class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('rules.list.exportMd') }}
            </DropdownItem>
          </template>
          <DropdownItem @click="openRulesFolder">
            <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('rules.editor.openFolder') }}
          </DropdownItem>
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

    <div v-else class="flex min-h-0 flex-1 flex-col px-5">
      <DetailSection :title="t('rules.detail.delivery')" :hint="t('rules.detail.deliveryHint')">
        <div class="flex flex-wrap items-center gap-x-5 gap-y-2">
          <label class="flex items-center gap-2">
            <span class="text-xs text-muted-foreground">{{ t('rules.editor.target') }}</span>
            <AppSelect
              :model-value="target"
              size="sm"
              class="w-32"
              :options="[
                { value: 'both', label: t('rules.editor.targetBoth') },
                { value: 'claude', label: t('rules.editor.targetClaude') },
                { value: 'codex', label: t('rules.editor.targetCodex') },
              ]"
              @update:model-value="(v: string) => setField('target', v)"
            />
          </label>
          <label v-if="target !== 'claude'" class="flex items-center gap-2" :title="t('rules.editor.codexModeHint')">
            <span class="text-xs text-muted-foreground">{{ t('rules.editor.codexMode') }}</span>
            <AppSelect
              :model-value="codexMode"
              size="sm"
              class="w-32"
              :options="[
                { value: 'inline', label: t('rules.editor.codexModeInline') },
                { value: 'skill', label: t('rules.editor.codexModeSkill') },
              ]"
              @update:model-value="(v: string) => setCodexMode(v as CodexMode)"
            />
          </label>
        </div>
        <p v-if="target !== 'claude'" class="text-[11px] text-muted-foreground/80">
          {{ t('rules.editor.codexModeHint') }}
        </p>
      </DetailSection>

      <DetailSection :title="t('rules.detail.content')" class="flex min-h-0 flex-1 flex-col">
        <template #actions>
          <Button variant="ghost" size="xs" :title="t('rules.editor.insertTemplate')" @click="editorRef?.insertTemplate()">
            <LayoutTemplate class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('rules.editor.template') }}
          </Button>
          <Button
            variant="ghost"
            size="xs"
            :aria-pressed="showPreview"
            @click="showPreview = !showPreview"
          >
            <component :is="showPreview ? EyeOff : Eye" class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ showPreview ? t('rules.detail.hidePreview') : t('rules.detail.showPreview') }}
          </Button>
        </template>
        <div class="flex min-h-0 flex-1 overflow-hidden rounded-md border border-border/60">
          <div class="min-w-0 flex-1">
            <SkillEditor
              ref="editorRef"
              v-model="content"
              :is-dark="true"
              :template="RULE_TEMPLATE"
            />
          </div>
          <div
            v-if="showPreview"
            v-mermaid
            v-copy-code
            class="min-w-0 flex-1 overflow-auto border-l border-border/60 p-5 text-sm leading-relaxed
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
      </DetailSection>
    </div>
  </section>
</template>
