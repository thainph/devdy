<script setup lang="ts">
// Right pane of the Skills screen: one skill (or a new draft when `skillId` is
// null). SKILL.md is the source of truth — the metadata fields rewrite its
// frontmatter and are re-read from it. The skill's extra files (references/,
// scripts/, …) are listed as tabs and shown read-only: the app has no command
// to write them, so editing goes through the folder (or the devdy MCP tools).
//
// The host keys this component on the selection, so every skill starts from a
// fresh editor (own undo history, own dirty baseline).
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { readDir, readTextFile, stat } from '@tauri-apps/plugin-fs'
import {
  AlertCircle, Columns2, Download, Eye, FileText, FolderCheck, FolderOpen, LayoutTemplate,
  MoreHorizontal, Pencil, RefreshCw, Save, Trash2,
} from 'lucide-vue-next'
import { AppSelect, Badge, Button, DropdownItem, DropdownMenu, DropdownSeparator, Input } from '@/components/ui'
import DetailHeader from '@/components/listDetail/DetailHeader.vue'
import DetailSection from '@/components/listDetail/DetailSection.vue'
import SkillEditor from '@/components/SkillEditor.vue'
import { useSkillsStore, type Skill, type SkillTarget } from '@/stores/skills'
import type { Group } from '@/stores/groups'
import { useToast } from '@/composables/useToast'
import { invoke } from '@/lib/tauri'
import { applyMermaidFence, vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import {
  SKILL_TEMPLATE, normalizeFrontmatter, parseFrontmatter, setFrontmatterField, validateFrontmatter,
} from '@/lib/skillRows'

const props = defineProps<{
  /** null = a new, unsaved skill. */
  skillId: string | null
  /** The store's row of the skill (meta line); null for a draft. */
  skill: Skill | null
  /** The skill's groups, for the meta line. */
  groups: Group[]
  applying: boolean
  /** Some re-apply is running (this skill's or all). */
  reapplyBusy: boolean
  /** This skill's re-apply is running. */
  reapplying: boolean
  deleting: boolean
}>()

const emit = defineEmits<{
  saved: [skill: Skill, created: boolean]
  /** The skill could not be loaded (deleted elsewhere). */
  missing: []
  applyAll: []
  reapply: []
  export: []
  delete: []
}>()

const { t } = useI18n()
const store = useSkillsStore()
const { toast } = useToast()

const isNew = computed(() => props.skillId === null)

const content = ref(SKILL_TEMPLATE)
/** Content as last loaded/saved — `dirty` compares against it. */
const original = ref(SKILL_TEMPLATE)
const sourcePath = ref<string | null>(null)
const loading = ref(false)
const saving = ref(false)
const editorRef = ref<InstanceType<typeof SkillEditor> | null>(null)

const dirty = computed(() => content.value !== original.value)
const fm = computed(() => parseFrontmatter(content.value))
const validation = computed(() => validateFrontmatter(content.value))
const validationText = computed(() =>
  validation.value ? t(`skills.editor.${validation.value.key}`, validation.value.params ?? {}) : '',
)
const canSave = computed(() => dirty.value && !saving.value && !loading.value && !validation.value)

const targetOptions = computed(() => [
  { value: 'both', label: t('skills.editor.targetBoth') },
  { value: 'claude', label: t('skills.editor.targetClaude') },
  { value: 'codex', label: t('skills.editor.targetCodex') },
])
const targetLabel = computed(
  () => targetOptions.value.find((o) => o.value === (fm.value.target || 'both'))?.label ?? fm.value.target,
)

// ── Metadata fields ─────────────────────────────────────────────────────────
// Local drafts: the frontmatter stores trimmed values, so binding the inputs to
// the parsed values directly would eat a space while it is being typed.
const nameDraft = ref('')
const descriptionDraft = ref('')

watch(fm, (v) => {
  if (v.name !== nameDraft.value.trim()) nameDraft.value = v.name
  if (v.description !== descriptionDraft.value.trim()) descriptionDraft.value = v.description
}, { immediate: true })

function setField(key: 'name' | 'description', value: string) {
  if (key === 'name') nameDraft.value = value
  else descriptionDraft.value = value
  content.value = setFrontmatterField(content.value, key, value.trim())
}

function setTarget(value: string) {
  content.value = setFrontmatterField(content.value, 'target', value as SkillTarget)
}

// ── Preview ─────────────────────────────────────────────────────────────────

type ViewMode = 'edit' | 'split' | 'preview'
const VIEW_KEY = 'devdy.skills.view'
const viewMode = ref<ViewMode>(readViewMode())
const viewModes = computed(() => [
  { value: 'edit' as const, label: t('skills.editor.markdown'), icon: Pencil },
  { value: 'split' as const, label: t('skills.detail.split'), icon: Columns2 },
  { value: 'preview' as const, label: t('skills.editor.preview'), icon: Eye },
])

function readViewMode(): ViewMode {
  try {
    const v = localStorage.getItem(VIEW_KEY)
    return v === 'split' || v === 'preview' ? v : 'edit'
  } catch {
    return 'edit'
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

// ── Extra files ─────────────────────────────────────────────────────────────

const MAX_FILES = 200
const MAX_PREVIEW_BYTES = 512 * 1024
const SKILL_MD = 'SKILL.md'

const files = ref<string[]>([])
const activeFile = ref(SKILL_MD)
const fileText = ref<string | null>(null)
/** Why `fileText` is null: a message key, or '' while loading. */
const fileIssue = ref('')

async function walk(root: string, rel: string, out: string[]) {
  const entries = await readDir(rel ? `${root}/${rel}` : root)
  entries.sort((a, b) => a.name.localeCompare(b.name))
  for (const e of entries) {
    if (out.length >= MAX_FILES) return
    if (e.name.startsWith('.')) continue
    const path = rel ? `${rel}/${e.name}` : e.name
    if (e.isDirectory) await walk(root, path, out)
    else if (path !== SKILL_MD) out.push(path)
  }
}

async function loadFiles() {
  const root = sourcePath.value
  if (!root) {
    files.value = []
    return
  }
  const out: string[] = []
  try {
    await walk(root, '', out)
  } catch {
    /* outside the readable scope or gone: show SKILL.md only */
  }
  files.value = out
  if (activeFile.value !== SKILL_MD && !out.includes(activeFile.value)) activeFile.value = SKILL_MD
}

let fileSeq = 0
watch(activeFile, async (path) => {
  const seq = ++fileSeq
  fileText.value = null
  fileIssue.value = ''
  if (path === SKILL_MD || !sourcePath.value) return
  const full = `${sourcePath.value}/${path}`
  try {
    const info = await stat(full)
    if (info.size > MAX_PREVIEW_BYTES) {
      if (seq === fileSeq) fileIssue.value = 'skills.detail.fileTooLarge'
      return
    }
    const text = await readTextFile(full)
    if (seq !== fileSeq) return
    if (text.includes('\u0000')) fileIssue.value = 'skills.detail.fileBinary'
    else fileText.value = text
  } catch {
    if (seq === fileSeq) fileIssue.value = 'skills.detail.fileUnreadable'
  }
})

// ── Load / save ─────────────────────────────────────────────────────────────

onMounted(async () => {
  if (!props.skillId) return
  loading.value = true
  try {
    const sc = await store.getSkill(props.skillId)
    content.value = sc.content
    original.value = sc.content
    sourcePath.value = sc.skill.source_path
    void loadFiles()
  } catch (e) {
    toast.error(String(e))
    emit('missing')
  } finally {
    loading.value = false
  }
})

async function save() {
  if (!canSave.value) return
  // Auto-normalize multi-line frontmatter values before saving so the written
  // SKILL.md is always valid YAML; validation already blocks what can't be fixed.
  const norm = normalizeFrontmatter(content.value)
  if (norm.error) return
  if (norm.changed) content.value = norm.content
  if (validateFrontmatter(content.value)) return

  const meta = parseFrontmatter(content.value)
  const payload = {
    name: meta.name,
    description: meta.description,
    target: (meta.target || 'both') as SkillTarget,
    content: content.value,
  }
  saving.value = true
  try {
    const saved = props.skillId
      ? await store.updateSkill({ id: props.skillId, ...payload })
      : await store.createSkill(payload)
    original.value = payload.content
    // A rename moves the folder.
    if (saved.source_path !== sourcePath.value) {
      sourcePath.value = saved.source_path
      void loadFiles()
    }
    toast.success(t('skills.editor.toastSaved'))
    emit('saved', saved, !props.skillId)
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

function insertTemplate() {
  activeFile.value = SKILL_MD
  editorRef.value?.insertTemplate()
}

async function openFolder() {
  if (!props.skillId) return
  try {
    await invoke('open_skill_folder', { id: props.skillId })
  } catch (e) {
    toast.error(String(e))
  }
}

function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' })
}

defineExpose({ dirty, save })
</script>

<template>
  <section class="flex min-w-0 flex-1 flex-col" @keydown="onKeydown">
    <DetailHeader :title="fm.name" :placeholder="t('skills.detail.untitled')">
      <template #meta>
        <template v-if="isNew">{{ t('skills.detail.newDraft') }}</template>
        <template v-else>
          <span>{{ targetLabel }}</span>
          <template v-if="skill"> · {{ t('skills.detail.updated', { date: formatDate(skill.updated_at) }) }}</template>
          <template v-if="groups.length"> · {{ groups.map((g) => g.name).join(', ') }}</template>
        </template>
      </template>
      <template #actions>
        <Button size="sm" :disabled="!canSave" :title="t('skills.editor.save')" @click="save">
          <Save class="h-3.5 w-3.5" :stroke-width="2" />
          {{ saving ? t('skills.editor.saving') : t('common.save') }}
        </Button>
        <DropdownMenu>
          <template #trigger>
            <Button variant="ghost" size="icon-sm" :title="t('skills.detail.more')" :aria-label="t('skills.detail.more')">
              <MoreHorizontal class="h-4 w-4" :stroke-width="1.75" />
            </Button>
          </template>
          <DropdownItem :disabled="loading" @click="insertTemplate">
            <LayoutTemplate class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('skills.editor.insertTemplate') }}
          </DropdownItem>
          <template v-if="!isNew">
            <DropdownItem v-if="sourcePath" @click="openFolder">
              <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('skills.editor.openInFinder') }}
            </DropdownItem>
            <DropdownSeparator />
            <DropdownItem :disabled="applying" @click="emit('applyAll')">
              <FolderCheck class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('skills.list.applyToAll') }}
            </DropdownItem>
            <DropdownItem :disabled="reapplyBusy" @click="emit('reapply')">
              <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" :class="{ 'animate-spin': reapplying }" />
              {{ t('skills.list.reapply') }}
            </DropdownItem>
            <DropdownItem @click="emit('export')">
              <Download class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('skills.list.exportZip') }}
            </DropdownItem>
            <DropdownSeparator />
            <DropdownItem variant="destructive" :disabled="deleting" @click="emit('delete')">
              <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('common.delete') }}
            </DropdownItem>
          </template>
        </DropdownMenu>
      </template>
    </DetailHeader>

    <div v-if="loading" class="flex flex-1 items-center justify-center text-sm text-muted-foreground">
      {{ t('common.loading') }}
    </div>

    <template v-else>
      <div class="shrink-0 px-5">
        <DetailSection :title="t('skills.detail.details')" :hint="t('skills.detail.detailsHint')">
          <div class="grid grid-cols-[minmax(0,1fr)_8rem] gap-3">
            <label class="block space-y-1">
              <span class="text-[11px] font-medium text-muted-foreground">{{ t('skills.detail.name') }}</span>
              <Input
                :model-value="nameDraft"
                class="font-mono"
                placeholder="my-skill"
                @update:model-value="(v: string) => setField('name', v)"
              />
            </label>
            <label class="block space-y-1">
              <span class="text-[11px] font-medium text-muted-foreground">{{ t('skills.editor.target') }}</span>
              <AppSelect :model-value="fm.target || 'both'" :options="targetOptions" @update:model-value="setTarget" />
            </label>
            <label class="col-span-2 block space-y-1">
              <span class="text-[11px] font-medium text-muted-foreground">{{ t('skills.detail.description') }}</span>
              <Input
                :model-value="descriptionDraft"
                :placeholder="t('skills.detail.descriptionPlaceholder')"
                @update:model-value="(v: string) => setField('description', v)"
              />
            </label>
          </div>
        </DetailSection>
      </div>

      <!-- File tabs + view mode -->
      <div class="flex shrink-0 items-center gap-2 border-y border-border/60 bg-muted/30 px-3 py-1">
        <div class="flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto">
          <button
            v-for="path in [SKILL_MD, ...files]"
            :key="path"
            type="button"
            class="flex shrink-0 cursor-pointer items-center gap-1 rounded px-2 py-1 font-mono text-[11px] transition-colors"
            :class="activeFile === path
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground'"
            :title="path"
            @click="activeFile = path"
          >
            <FileText class="h-3 w-3 shrink-0" :stroke-width="1.75" />
            <span class="max-w-48 truncate">{{ path }}</span>
            <span v-if="path === SKILL_MD && dirty" class="h-1.5 w-1.5 rounded-full bg-primary" />
          </button>
        </div>
        <div v-if="activeFile === SKILL_MD" class="flex shrink-0 rounded-md bg-muted p-0.5" role="group">
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
        v-if="validation && activeFile === SKILL_MD"
        class="flex shrink-0 items-center gap-1.5 border-b border-amber-500/20 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-500"
      >
        <AlertCircle class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        <span class="min-w-0 truncate" :title="validationText">{{ validationText }}</span>
      </div>

      <!-- SKILL.md: kept mounted while another file is shown so its undo history survives. -->
      <div v-show="activeFile === SKILL_MD" class="flex min-h-0 flex-1">
        <div
          v-show="viewMode !== 'preview'"
          class="flex min-w-0 flex-1 flex-col overflow-hidden"
          :class="viewMode === 'split' && 'border-r border-border/60'"
        >
          <SkillEditor
            ref="editorRef"
            v-model="content"
            :is-dark="true"
            :template="SKILL_TEMPLATE"
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

      <!-- Any other file: read-only. -->
      <div v-if="activeFile !== SKILL_MD" class="flex min-h-0 flex-1 flex-col">
        <div class="flex shrink-0 items-center gap-2 border-b border-border/40 px-3 py-1.5 text-[11px] text-muted-foreground">
          <Badge tone="neutral" size="xs">{{ t('skills.detail.readOnly') }}</Badge>
          <span class="min-w-0 flex-1 truncate">{{ t('skills.detail.readOnlyHint') }}</span>
          <Button v-if="sourcePath" variant="outline" size="xs" @click="openFolder">
            <FolderOpen class="h-3 w-3" :stroke-width="1.75" />
            {{ t('skills.editor.openInFinder') }}
          </Button>
        </div>
        <pre
          v-if="fileText !== null"
          class="min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words p-4 font-mono text-xs leading-relaxed text-foreground/90"
        >{{ fileText }}</pre>
        <p v-else class="p-4 text-xs text-muted-foreground">
          {{ fileIssue ? t(fileIssue) : t('common.loading') }}
        </p>
      </div>
    </template>
  </section>
</template>
