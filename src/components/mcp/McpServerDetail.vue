<script setup lang="ts">
// Right pane of the MCP servers screen for a user server (or a new draft):
// header with the live name, meta (transport · enabled · added) and actions
// (enabled switch · Test · Save · ⋯), then the form in flat sections.
//
// A saved server's enabled switch applies at once (like the sidebar's power
// button), so it is not part of the dirty check; on a draft it is a form field.
// Secret VALUEs are write-only: stored keys come back without values, a blank
// value on a stored key keeps it (see lib/mcpRows toSecretEntries).
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertCircle, AlertTriangle, CheckCircle2, Copy, Download, MoreHorizontal, Plug, Plus, Server, Trash2, X, XCircle,
} from 'lucide-vue-next'
import {
  AppSelect, Badge, Button, DropdownItem, DropdownMenu, DropdownSeparator, Input, Textarea,
} from '@/components/ui'
import DetailHeader from '@/components/listDetail/DetailHeader.vue'
import DetailSection from '@/components/listDetail/DetailSection.vue'
import { useToast } from '@/composables/useToast'
import {
  useMcpServersStore,
  type McpServer,
  type McpTransport,
  type TestConnectionResult,
} from '@/stores/mcpServers'
import {
  blankForm,
  formFromServer,
  formPayload,
  formSnapshot,
  isRemote,
  validateForm,
  validateTarget,
  type FormError,
  type McpForm,
} from '@/lib/mcpRows'

const props = defineProps<{
  /** null = a new draft. */
  server: McpServer | null
  /** A draft's prefill (Duplicate). */
  seed?: McpForm | null
  defaultIsCodex: boolean
  /** The enabled switch is being applied. */
  toggling: boolean
}>()

const emit = defineEmits<{
  /** Saved; a created server reports its new id. */
  saved: [id: string]
  toggleEnabled: []
  export: []
  duplicate: []
  delete: []
}>()

const { t, locale } = useI18n()
const { toast } = useToast()
const store = useMcpServersStore()

const isNew = computed(() => props.server === null)

const form = ref<McpForm>(props.server ? formFromServer(props.server) : { ...blankForm(), ...props.seed })
// A draft compares against a blank form, so a prefilled one can be saved at once.
const baseline = ref(formSnapshot(props.server ? formFromServer(props.server) : blankForm(), isNew.value))

const dirty = computed(() => formSnapshot(form.value, isNew.value) !== baseline.value)

const saving = ref(false)
const testing = ref(false)
const testResult = ref<TestConnectionResult | null>(null)
const validationError = ref<string | null>(null)

const remote = computed(() => isRemote(form.value.transport))
const isSse = computed(() => form.value.transport === 'sse')
/** The switch shows the saved state for a server, the draft's own otherwise. */
const enabled = computed(() => props.server?.enabled ?? form.value.enabled)

// stdio (local process) / http (remote) / sse (remote) are technical protocol
// tokens and stay verbatim.
const transportOptions = [
  { value: 'stdio', label: 'stdio (local process)' },
  { value: 'http', label: 'http (remote)' },
  { value: 'sse', label: 'sse (remote)' },
]

function seedFrom(server: McpServer) {
  form.value = formFromServer(server)
  baseline.value = formSnapshot(form.value, false)
}

// The store refetched (a sidebar toggle, a save, a refresh): follow it unless
// the user is mid-edit — then only the enabled state, which isn't theirs.
watch(
  () => props.server,
  (server) => {
    if (!server) return
    if (dirty.value) form.value.enabled = server.enabled
    else seedFrom(server)
  },
)

function errorText(err: FormError): string {
  return t(`mcp.editor.validation.${err.key}`, err.params ?? {})
}

function toggleEnabled() {
  if (isNew.value) form.value.enabled = !form.value.enabled
  else emit('toggleEnabled')
}

function addEnvRow() {
  form.value.envRows.push({ key: '', value: '', existing: false })
}
function addHeaderRow() {
  form.value.headerRows.push({ key: '', value: '', existing: false })
}

// Tests the form as currently filled in — no save needed, new or existing.
async function handleTest() {
  const err = validateTarget(form.value)
  if (err) {
    testResult.value = { ok: false, message: errorText(err) }
    return
  }
  testing.value = true
  testResult.value = null
  const p = formPayload(form.value)
  try {
    testResult.value = await store.testConnection({
      // Lets omitted secret VALUEs fall back to the stored ones.
      id: props.server?.id,
      transport: p.transport,
      command: p.command,
      args: p.args,
      url: p.url,
      env: p.env,
      headers: p.headers,
    })
  } catch (e) {
    testResult.value = { ok: false, message: String(e) }
  } finally {
    testing.value = false
  }
}

async function save() {
  if (!dirty.value || saving.value) return
  const err = validateForm(form.value)
  if (err) {
    validationError.value = errorText(err)
    return
  }
  validationError.value = null
  saving.value = true
  try {
    const payload = formPayload(form.value)
    if (props.server) {
      const saved = await store.updateServer({ ...payload, id: props.server.id, enabled: props.server.enabled })
      // Typed secret values are stored now: back to key-only rows.
      seedFrom(saved)
      emit('saved', saved.id)
    } else {
      const created = await store.createServer(payload)
      baseline.value = formSnapshot(form.value, true)
      emit('saved', created.id)
    }
    toast.success(t('mcp.editor.toast.saved'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

const meta = computed(() => {
  const parts = [form.value.transport, enabled.value ? t('mcp.editor.enabled') : t('mcp.editor.disabled')]
  parts.push(
    props.server
      ? t('mcp.detail.addedOn', {
          date: new Date(props.server.created_at).toLocaleDateString(locale.value, {
            month: 'short',
            day: 'numeric',
            year: 'numeric',
          }),
        })
      : t('mcp.detail.notSaved'),
  )
  return parts.join(' · ')
})

function onKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') {
    e.preventDefault()
    void save()
  }
}

defineExpose({ dirty })
</script>

<template>
  <section class="flex min-w-0 flex-1 flex-col" @keydown="onKeydown">
    <DetailHeader :title="form.name.trim() || (server?.name ?? '')" :placeholder="t('mcp.editor.newTitle')">
      <template #leading>
        <Server class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="1.75" />
      </template>
      <template #meta>
        {{ meta }}<template v-if="isSse && defaultIsCodex">
          ·
          <span class="font-medium text-amber-600 dark:text-amber-400" :title="t('mcp.list.claudeOnlyTitle')">
            {{ t('mcp.list.claudeOnly') }}
          </span>
        </template>
      </template>
      <template #actions>
        <!-- Validation error inline -->
        <div
          v-if="validationError"
          class="flex min-w-0 items-center gap-1 rounded-md border border-amber-500/20 bg-amber-500/10 px-2.5 py-1 text-xs text-amber-500"
          :title="validationError"
        >
          <AlertCircle class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span class="max-w-48 truncate">{{ validationError }}</span>
        </div>
        <button
          type="button"
          class="flex shrink-0 cursor-pointer select-none items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors hover:bg-accent disabled:cursor-default disabled:opacity-50"
          :aria-pressed="enabled"
          :disabled="toggling"
          :title="enabled ? t('mcp.list.disable') : t('mcp.list.enable')"
          @click="toggleEnabled"
        >
          <span
            class="relative inline-flex h-4 w-7 items-center rounded-full transition-colors"
            :class="enabled ? 'bg-primary' : 'bg-muted'"
          >
            <span
              class="inline-block h-3 w-3 transform rounded-full bg-white transition-transform"
              :class="enabled ? 'translate-x-3.5' : 'translate-x-0.5'"
            />
          </span>
          <span class="text-muted-foreground">{{ enabled ? t('mcp.editor.enabled') : t('mcp.editor.disabled') }}</span>
        </button>
        <Button variant="outline" size="xs" class="shrink-0" :disabled="testing" @click="handleTest">
          <Plug class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ testing ? t('mcp.editor.testing') : t('mcp.editor.testConnection') }}
        </Button>
        <Button size="xs" class="shrink-0" :disabled="!dirty || saving" @click="save">
          {{ saving ? t('mcp.editor.saving') : t('common.save') }}
        </Button>
        <DropdownMenu v-if="server" align="right" class="shrink-0">
          <template #trigger>
            <Button
              variant="outline"
              size="icon-sm"
              :title="t('mcp.detail.moreActions')"
              :aria-label="t('mcp.detail.moreActions')"
            >
              <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
          <DropdownItem @click="emit('export')">
            <Download class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('mcp.list.exportTitle') }}
          </DropdownItem>
          <DropdownItem @click="emit('duplicate')">
            <Copy class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('mcp.detail.duplicate') }}
          </DropdownItem>
          <DropdownSeparator />
          <DropdownItem variant="destructive" @click="emit('delete')">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('common.delete') }}
          </DropdownItem>
        </DropdownMenu>
      </template>
    </DetailHeader>

    <div class="min-h-0 flex-1 overflow-y-auto px-5">
      <div class="max-w-xl">
        <!-- Test connection result -->
        <div
          v-if="testResult"
          class="mt-4 flex items-start gap-1.5 rounded-md border p-3 text-xs"
          :class="testResult.ok
            ? 'border-emerald-500/20 bg-emerald-500/10 text-emerald-500'
            : 'border-destructive/20 bg-destructive/10 text-destructive'"
        >
          <CheckCircle2 v-if="testResult.ok" class="mt-px h-3.5 w-3.5 shrink-0" :stroke-width="2" />
          <XCircle v-else class="mt-px h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
          <span class="min-w-0 flex-1 break-words">{{ testResult.message }}</span>
          <button
            type="button"
            class="shrink-0 cursor-pointer opacity-70 hover:opacity-100"
            :title="t('mcp.detail.dismiss')"
            :aria-label="t('mcp.detail.dismiss')"
            @click="testResult = null"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
        </div>

        <DetailSection :title="t('mcp.editor.general')" :hint="t('mcp.detail.generalHint')">
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground">{{ t('mcp.editor.name') }}</label>
            <Input v-model="form.name" size="sm" :placeholder="t('mcp.editor.namePlaceholder')" class="font-mono" />
          </div>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground">{{ t('mcp.editor.description') }}</label>
            <Input v-model="form.description" size="sm" :placeholder="t('mcp.editor.descriptionPlaceholder')" />
          </div>
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground">{{ t('mcp.editor.transport') }}</label>
            <AppSelect
              size="sm"
              :model-value="form.transport"
              :options="transportOptions"
              @update:model-value="(v: string) => (form.transport = v as McpTransport)"
            />
            <p v-if="isSse" class="flex items-center gap-1 text-[10px] text-amber-500">
              <AlertTriangle class="h-3 w-3 shrink-0" :stroke-width="1.75" />
              {{ t('mcp.editor.sseWarning') }}
            </p>
          </div>
        </DetailSection>

        <!-- stdio: the process to spawn -->
        <template v-if="!remote">
          <DetailSection :title="t('mcp.editor.process')">
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground">{{ t('mcp.editor.command') }}</label>
              <Input v-model="form.command" size="sm" :placeholder="t('mcp.editor.commandPlaceholder')" class="font-mono" />
            </div>
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground">{{ t('mcp.editor.args') }}</label>
              <Textarea
                v-model="form.argsText"
                rows="3"
                placeholder="-y&#10;@modelcontextprotocol/server-filesystem"
                class="font-mono"
              />
            </div>
          </DetailSection>

          <DetailSection :title="t('mcp.editor.environment')" :hint="t('mcp.detail.secretsHint')">
            <template #actions>
              <Button variant="outline" size="xs" @click="addEnvRow">
                <Plus class="h-3 w-3" :stroke-width="2" />
                {{ t('common.add') }}
              </Button>
            </template>
            <div v-if="form.envRows.length === 0" class="text-[11px] text-muted-foreground">{{ t('mcp.editor.noEnv') }}</div>
            <div v-for="(row, i) in form.envRows" :key="i" class="flex items-center gap-1.5">
              <Input v-model="row.key" size="sm" :placeholder="t('mcp.editor.keyPlaceholder')" class="flex-1 font-mono" />
              <Input
                v-model="row.value"
                size="sm"
                type="password"
                autocomplete="off"
                :placeholder="row.existing ? t('mcp.editor.unchangedPlaceholder') : t('mcp.editor.valuePlaceholder')"
                class="flex-1 font-mono"
              />
              <Badge v-if="row.existing" tone="neutral" size="xs" class="shrink-0" :title="t('mcp.editor.storedTitle')">
                {{ t('mcp.editor.stored') }}
              </Badge>
              <Button
                variant="destructive-ghost"
                size="icon-sm"
                class="shrink-0"
                :title="t('common.remove')"
                :aria-label="t('common.remove')"
                @click="form.envRows.splice(i, 1)"
              >
                <Trash2 class="h-3 w-3" :stroke-width="1.75" />
              </Button>
            </div>
          </DetailSection>
        </template>

        <!-- http / sse: the endpoint -->
        <template v-else>
          <DetailSection :title="t('mcp.editor.endpoint')">
            <div class="space-y-1.5">
              <label class="text-[11px] font-medium text-muted-foreground">{{ t('mcp.editor.url') }}</label>
              <Input v-model="form.url" size="sm" :placeholder="t('mcp.editor.urlPlaceholder')" class="font-mono" />
            </div>
          </DetailSection>

          <DetailSection :title="t('mcp.editor.headers')" :hint="t('mcp.detail.secretsHint')">
            <template #actions>
              <Button variant="outline" size="xs" @click="addHeaderRow">
                <Plus class="h-3 w-3" :stroke-width="2" />
                {{ t('common.add') }}
              </Button>
            </template>
            <div v-if="form.headerRows.length === 0" class="text-[11px] text-muted-foreground">{{ t('mcp.editor.noHeaders') }}</div>
            <div v-for="(row, i) in form.headerRows" :key="i" class="flex items-center gap-1.5">
              <Input v-model="row.key" size="sm" :placeholder="t('mcp.editor.authorizationPlaceholder')" class="flex-1 font-mono" />
              <Input
                v-model="row.value"
                size="sm"
                type="password"
                autocomplete="off"
                :placeholder="row.existing ? t('mcp.editor.unchangedPlaceholder') : t('mcp.editor.valuePlaceholder')"
                class="flex-1 font-mono"
              />
              <Badge v-if="row.existing" tone="neutral" size="xs" class="shrink-0" :title="t('mcp.editor.storedTitle')">
                {{ t('mcp.editor.stored') }}
              </Badge>
              <Button
                variant="destructive-ghost"
                size="icon-sm"
                class="shrink-0"
                :title="t('common.remove')"
                :aria-label="t('common.remove')"
                @click="form.headerRows.splice(i, 1)"
              >
                <Trash2 class="h-3 w-3" :stroke-width="1.75" />
              </Button>
            </div>
          </DetailSection>
        </template>
      </div>
    </div>
  </section>
</template>
