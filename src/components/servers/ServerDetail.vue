<script setup lang="ts">
// Right pane of the Servers screen: one server's form (or a new draft when
// `server` is null), edited inline and saved with Save / ⌘/Ctrl+S. The header
// title follows the Label field, like the Skills pane.
// The host keys this component per selection, so the form is seeded once from
// the props and later store refreshes (e.g. a test updating the status) never
// overwrite what is being typed. Tests and deletes go through the host, which
// also remembers each server's last test result while the screen lives.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { open } from '@tauri-apps/plugin-dialog'
import {
  AlertCircle, CheckCircle2, Copy, CopyPlus, KeyRound, Loader2, MoreHorizontal, Plug, Trash2, Upload, X, XCircle,
} from 'lucide-vue-next'
import { Banner, Button, DropdownItem, DropdownMenu, DropdownSeparator, Input, SegmentedControl } from '@/components/ui'
import DetailHeader from '@/components/listDetail/DetailHeader.vue'
import DetailSection from '@/components/listDetail/DetailSection.vue'
import { useToast } from '@/composables/useToast'
import { relativeAge } from '@/lib/itemText'
import { splitTags } from '@/lib/listGroups'
import { addTags, joinTags, serverStatus, sshCommand } from '@/lib/serverRows'
import {
  useServersStore,
  type AuthMethod,
  type TestConnectionResult,
  type VpsServer,
} from '@/stores/servers'

export type ServerPrefill = Pick<VpsServer, 'label' | 'host' | 'port' | 'username' | 'auth_method' | 'tags'>

type Field = 'label' | 'host' | 'port' | 'username' | 'key'

const props = withDefaults(
  defineProps<{
    /** null = a new, unsaved draft. */
    server: VpsServer | null
    /** Seeds a new draft (Duplicate). */
    prefill?: ServerPrefill | null
    testResult?: TestConnectionResult | null
    testing?: boolean
    deleting?: boolean
  }>(),
  { prefill: null, testResult: null, testing: false, deleting: false },
)

const emit = defineEmits<{
  saved: [server: VpsServer, created: boolean]
  test: []
  delete: []
  duplicate: []
}>()

const store = useServersStore()
const { toast } = useToast()
const { t } = useI18n()

const isNew = computed(() => !props.server)

const label = ref('')
const host = ref('')
const port = ref('22')
const username = ref('')
const authMethod = ref<AuthMethod>('agent')
const privateKeyPath = ref('')
const privateKeySourcePath = ref('')
const tags = ref<string[]>([])
const tagInput = ref('')
const passphrase = ref('')
// True for an existing server that already has a stored passphrase; leaving
// the field blank then keeps it (BR-005 / AC-008).
const hasStoredPassphrase = ref(false)

const saving = ref(false)
const validationError = ref<{ field: Field; message: string } | null>(null)

const isKey = computed(() => authMethod.value === 'key')

function snapshot(): string {
  return JSON.stringify([
    label.value, host.value, String(port.value), username.value, authMethod.value,
    privateKeyPath.value, privateKeySourcePath.value, joinTags(tags.value), passphrase.value,
  ])
}

function applyServer(s: ServerPrefill & Partial<VpsServer>) {
  label.value = s.label
  host.value = s.host
  port.value = String(s.port)
  username.value = s.username
  authMethod.value = s.auth_method
  privateKeyPath.value = s.private_key_path ?? ''
  privateKeySourcePath.value = ''
  tags.value = splitTags(s.tags)
  tagInput.value = ''
  passphrase.value = ''
  hasStoredPassphrase.value = s.has_passphrase ?? false
}

// A new draft's baseline is the blank form, so a duplicate's prefilled values
// count as changes and can be saved straight away.
const baseline = ref(snapshot())
if (props.server) {
  applyServer(props.server)
  baseline.value = snapshot()
} else if (props.prefill) {
  applyServer(props.prefill)
}

const dirty = computed(() => snapshot() !== baseline.value || tagInput.value.trim() !== '')
const canSave = computed(() => dirty.value && !saving.value)

defineExpose({ dirty })

// Outside edits (another window, the AI via the devdy MCP tools) refresh a
// pane with nothing unsaved, so the next save doesn't overwrite them.
watch(
  () => props.server,
  (s) => {
    if (!s || dirty.value || saving.value) return
    applyServer(s)
    baseline.value = snapshot()
  },
)

// ── Private key ─────────────────────────────────────────────────────────────

function basename(path: string): string {
  const clean = path.trim().replace(/[/\\]+$/, '')
  if (!clean) return ''
  const slash = Math.max(clean.lastIndexOf('/'), clean.lastIndexOf('\\'))
  return slash >= 0 ? clean.slice(slash + 1) : clean
}

const selectedPrivateKeyName = computed(() => basename(privateKeySourcePath.value))
const storedPrivateKeyName = computed(() => basename(privateKeyPath.value))
const privateKeyName = computed(() => selectedPrivateKeyName.value || storedPrivateKeyName.value)
const privateKeyStateLabel = computed(() => {
  if (selectedPrivateKeyName.value) return t('servers.editor.selectedKeyFile')
  if (storedPrivateKeyName.value) return t('servers.editor.storedKeyFile')
  return t('servers.editor.noPrivateKey')
})

const authOptions = computed<{ value: AuthMethod; label: string }[]>(() => [
  { value: 'agent', label: t('servers.editor.authOptionAgent') },
  { value: 'key', label: t('servers.editor.authOptionKey') },
])

async function handleChoosePrivateKey() {
  const selected = await open({
    multiple: false,
    title: t('servers.editor.selectKeyDialog'),
  })
  const path = Array.isArray(selected) ? selected[0] : selected
  if (path) privateKeySourcePath.value = path
}

// ── Tags ────────────────────────────────────────────────────────────────────

function commitTagInput() {
  tags.value = addTags(tags.value, tagInput.value)
  tagInput.value = ''
}

function onTagKeydown(e: KeyboardEvent) {
  if (e.isComposing || e.keyCode === 229) return
  if (e.key === 'Enter' || e.key === ',') {
    e.preventDefault()
    commitTagInput()
  } else if (e.key === 'Backspace' && !tagInput.value && tags.value.length > 0) {
    tags.value = tags.value.slice(0, -1)
  }
}

function removeTag(tag: string) {
  tags.value = tags.value.filter((x) => x !== tag)
}

// ── Validation + save ───────────────────────────────────────────────────────

function validate(): { field: Field; message: string } | null {
  if (!label.value.trim()) return { field: 'label', message: t('servers.editor.validation.labelRequired') }
  if (!host.value.trim()) return { field: 'host', message: t('servers.editor.validation.hostRequired') }
  if (!username.value.trim()) return { field: 'username', message: t('servers.editor.validation.usernameRequired') }
  const p = Number(port.value)
  if (!Number.isInteger(p) || p < 1 || p > 65535) {
    return { field: 'port', message: t('servers.editor.validation.portRange') }
  }
  if (isKey.value && !privateKeyPath.value.trim() && !privateKeySourcePath.value.trim()) {
    return { field: 'key', message: t('servers.editor.validation.keyRequired') }
  }
  return null
}

// A shown error goes away (or moves on) as soon as the form is fixed.
watch(snapshot, () => {
  if (validationError.value) validationError.value = validate()
})

function fieldError(field: Field): string | null {
  return validationError.value?.field === field ? validationError.value.message : null
}

async function handleSave() {
  if (!canSave.value) return
  if (tagInput.value.trim()) commitTagInput()
  const err = validate()
  if (err) {
    validationError.value = err
    return
  }
  validationError.value = null
  saving.value = true

  const base = {
    label: label.value.trim(),
    host: host.value.trim(),
    port: Number(port.value),
    username: username.value.trim(),
    auth_method: authMethod.value,
    private_key_path: isKey.value ? privateKeyPath.value.trim() : null,
    private_key_source_path: isKey.value && privateKeySourcePath.value.trim()
      ? privateKeySourcePath.value.trim()
      : null,
    tags: joinTags(tags.value),
    // Omitted when blank so an existing secret is kept untouched.
    passphrase: passphrase.value.length > 0 ? passphrase.value : null,
  }
  try {
    if (props.server) {
      const updated = await store.updateServer({ id: props.server.id, ...base })
      applyServer(updated)
      baseline.value = snapshot()
      toast.success(t('servers.editor.toast.saved'))
      emit('saved', updated, false)
    } else {
      const created = await store.createServer(base)
      baseline.value = snapshot()
      toast.success(t('servers.editor.toast.saved'))
      emit('saved', created, true)
    }
  } catch (e) {
    toast.error(String(e))
  } finally {
    saving.value = false
  }
}

function onKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') {
    e.preventDefault()
    void handleSave()
  }
}

// ── Header ──────────────────────────────────────────────────────────────────

const meta = computed(() => {
  const s = props.server
  if (!s) return t('servers.detail.unsavedMeta')
  const status = serverStatus(s)
  const parts = [
    `${s.username}@${s.host}:${s.port}`,
    status === 'unknown' ? t('servers.list.status.notChecked') : t(`servers.list.status.${status}`),
  ]
  if (s.last_checked_at) parts.push(t('servers.detail.checked', { age: relativeAge(s.last_checked_at) }))
  return parts.join(' · ')
})

async function copySsh() {
  if (!props.server) return
  try {
    await navigator.clipboard.writeText(sshCommand(props.server))
    toast.success(t('servers.detail.sshCopied'))
  } catch (e) {
    toast.error(String(e))
  }
}

const labelClass = 'text-[11px] font-medium text-muted-foreground uppercase tracking-wider'
</script>

<template>
  <section class="flex min-w-0 flex-1 flex-col" @keydown="onKeydown">
    <DetailHeader :title="label" :placeholder="t('servers.detail.newTitle')">
      <template #meta>{{ meta }}</template>
      <template #actions>
        <Button size="sm" :disabled="!canSave" :title="t('servers.detail.saveHint')" @click="handleSave">
          {{ saving ? t('servers.editor.saving') : t('common.save') }}
        </Button>
        <DropdownMenu v-if="server" align="right">
          <template #trigger>
            <Button variant="outline" size="icon" :title="t('servers.detail.moreActions')" :aria-label="t('servers.detail.moreActions')">
              <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
          <DropdownItem @click="copySsh">
            <Copy class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('servers.detail.copySsh') }}
          </DropdownItem>
          <DropdownItem @click="emit('duplicate')">
            <CopyPlus class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('servers.detail.duplicate') }}
          </DropdownItem>
          <DropdownSeparator />
          <DropdownItem variant="destructive" :disabled="deleting" @click="emit('delete')">
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('common.delete') }}
          </DropdownItem>
        </DropdownMenu>
      </template>
    </DetailHeader>

    <div class="min-h-0 flex-1 overflow-y-auto px-5">
      <div class="max-w-2xl">
        <!-- Connection -->
        <DetailSection :title="t('servers.editor.connection')">
          <div class="space-y-1.5">
            <label :class="labelClass">{{ t('servers.editor.label') }}</label>
            <Input v-model="label" size="sm" :placeholder="t('servers.editor.labelPlaceholder')" />
            <p v-if="fieldError('label')" class="text-[11px] text-destructive">{{ fieldError('label') }}</p>
          </div>
          <div class="flex gap-3">
            <div class="flex-1 space-y-1.5">
              <label :class="labelClass">{{ t('servers.editor.host') }}</label>
              <Input v-model="host" size="sm" :placeholder="t('servers.editor.hostPlaceholder')" class="font-mono" />
              <p v-if="fieldError('host')" class="text-[11px] text-destructive">{{ fieldError('host') }}</p>
            </div>
            <div class="w-24 space-y-1.5">
              <label :class="labelClass">{{ t('servers.editor.port') }}</label>
              <Input v-model="port" size="sm" type="number" placeholder="22" class="font-mono" />
            </div>
          </div>
          <p v-if="fieldError('port')" class="-mt-1.5 text-[11px] text-destructive">{{ fieldError('port') }}</p>
          <div class="space-y-1.5">
            <label :class="labelClass">{{ t('servers.editor.username') }}</label>
            <Input v-model="username" size="sm" :placeholder="t('servers.editor.usernamePlaceholder')" class="font-mono" />
            <p v-if="fieldError('username')" class="text-[11px] text-destructive">{{ fieldError('username') }}</p>
          </div>
        </DetailSection>

        <!-- Authentication -->
        <DetailSection :title="t('servers.editor.authentication')">
          <div class="space-y-1.5">
            <SegmentedControl v-model="authMethod" :options="authOptions" class="max-w-md" />
            <p v-if="!isKey" class="flex items-center gap-1 text-[10px] text-muted-foreground">
              <AlertCircle class="h-3 w-3 shrink-0" :stroke-width="1.75" />
              {{ t('servers.editor.agentHint') }}
            </p>
          </div>
          <div v-if="isKey" class="space-y-1.5">
            <label :class="labelClass">{{ t('servers.editor.privateKeyFile') }}</label>
            <div class="flex items-center gap-2">
              <div class="flex h-8 min-w-0 flex-1 items-center gap-2 rounded-md border border-border bg-muted/30 px-2.5 text-xs">
                <KeyRound class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                <div class="min-w-0 flex-1">
                  <div class="truncate font-mono" :class="privateKeyName ? 'text-foreground' : 'text-muted-foreground'">
                    {{ privateKeyName || privateKeyStateLabel }}
                  </div>
                </div>
                <span
                  v-if="privateKeyName"
                  class="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground"
                >
                  {{ privateKeyStateLabel }}
                </span>
              </div>
              <Button variant="outline" size="sm" @click="handleChoosePrivateKey">
                <Upload class="h-3.5 w-3.5" :stroke-width="1.75" />
                {{ privateKeyName ? t('servers.editor.change') : t('servers.editor.choose') }}
              </Button>
              <Button
                v-if="privateKeySourcePath"
                variant="ghost"
                size="icon-sm"
                :title="t('servers.editor.clearSelectedKey')"
                @click="privateKeySourcePath = ''"
              >
                <X class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
            </div>
            <p v-if="fieldError('key')" class="text-[11px] text-destructive">{{ fieldError('key') }}</p>
            <p class="text-[10px] text-muted-foreground">
              {{ t('servers.editor.keyCopiedHint') }}
            </p>
          </div>
          <div v-if="isKey" class="space-y-1.5">
            <label :class="labelClass">{{ t('servers.editor.passphrase') }}</label>
            <Input
              v-model="passphrase"
              size="sm"
              type="password"
              :placeholder="hasStoredPassphrase ? t('servers.editor.passphraseUnchanged') : t('servers.editor.passphraseOptional')"
              class="font-mono"
            />
            <p class="text-[10px] text-muted-foreground">
              {{ t('servers.editor.passphraseHint') }}
            </p>
          </div>
        </DetailSection>

        <!-- Tags -->
        <DetailSection :title="t('servers.editor.tags')" :hint="t('servers.detail.tagsHint')">
          <div
            class="flex min-h-8 flex-wrap items-center gap-1 rounded-md border border-border bg-background px-1.5 py-1 focus-within:ring-1 focus-within:ring-ring"
          >
            <span
              v-for="tag in tags"
              :key="tag"
              class="flex items-center gap-0.5 rounded bg-muted py-0.5 pl-1.5 pr-0.5 text-[11px]"
            >
              {{ tag }}
              <button
                type="button"
                class="flex h-4 w-4 cursor-pointer items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
                :title="t('common.remove')"
                :aria-label="t('common.remove')"
                @click="removeTag(tag)"
              >
                <X class="h-2.5 w-2.5" :stroke-width="2" />
              </button>
            </span>
            <input
              v-model="tagInput"
              class="h-6 min-w-24 flex-1 bg-transparent px-1 text-xs outline-none placeholder:text-muted-foreground"
              :placeholder="tags.length ? '' : t('servers.detail.tagsPlaceholder')"
              @keydown="onTagKeydown"
              @blur="commitTagInput"
            >
          </div>
        </DetailSection>

        <!-- Test connection (same row layout as the MCP detail's Status section):
             tests the saved server, so a draft has to be saved first. -->
        <DetailSection :title="t('servers.list.testTitle')">
          <div class="flex items-center gap-3">
            <div class="min-w-0 flex-1">
              <div class="text-xs font-medium">{{ t('servers.detail.lastTest') }}</div>
              <p class="text-[11px] text-muted-foreground">
                {{ isNew ? t('servers.editor.saveFirstThenTest') : testing ? t('servers.editor.testing') : testResult ? t('servers.detail.testHint') : t('servers.detail.notTested') }}
              </p>
            </div>
            <Button
              variant="outline"
              size="xs"
              class="shrink-0"
              :disabled="isNew || testing"
              :title="isNew ? t('servers.editor.saveFirst') : t('servers.list.testTitle')"
              @click="emit('test')"
            >
              <Loader2 v-if="testing" class="h-3 w-3 animate-spin" :stroke-width="2" />
              <Plug v-else class="h-3 w-3" :stroke-width="1.75" />
              {{ testing ? t('servers.editor.testing') : t('servers.detail.test') }}
            </Button>
          </div>
          <Banner
            v-if="!isNew && testResult"
            :tone="testResult.ok ? 'success' : 'destructive'"
            :icon="testResult.ok ? CheckCircle2 : XCircle"
          >
            {{ testResult.message }}
          </Banner>
        </DetailSection>
      </div>
    </div>
  </section>
</template>
