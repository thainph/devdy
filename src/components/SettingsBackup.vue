<script setup lang="ts">
// Settings → "Backup & Sync": export the whole Devdy configuration (settings, Skills/Rules
// library, MCP + VPS servers, account metadata, notes/todos) into one file and import it on
// another machine. Secrets only travel when the user sets a password (sealed backend-side with
// Argon2id + XChaCha20-Poly1305). Import merges by name and never deletes local items.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { open, save } from '@tauri-apps/plugin-dialog'
import { AlertTriangle, ArchiveRestore, Download, FileUp, KeyRound, Loader2, RotateCcw, Upload } from 'lucide-vue-next'
import { Button, Card, Input } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { invoke } from '@/lib/tauri'

interface BackupInfo {
  exportedAt: string
  appVersion: string
  hasSecrets: boolean
  counts: Record<string, number>
}

interface ImportCounts {
  created: number
  updated: number
  unchanged: number
}

interface ImportReport {
  counts: Record<string, ImportCounts>
  secretsImported: boolean
  warnings: string[]
}

const MIN_PASSWORD = 8
const FILE_EXT = 'devdy-backup'

const { t } = useI18n()
const { toast } = useToast()

// ---- Export ----
const includeSecrets = ref(false)
const exportPassword = ref('')
const exportPasswordConfirm = ref('')
const exporting = ref(false)

const exportError = computed(() => {
  if (!includeSecrets.value) return ''
  if (exportPassword.value.length < MIN_PASSWORD) return t('settings.backup.export.passwordTooShort', { n: MIN_PASSWORD })
  if (exportPassword.value !== exportPasswordConfirm.value) return t('settings.backup.export.passwordMismatch')
  return ''
})

async function runExport() {
  if (exporting.value || exportError.value) return
  const stamp = new Date().toISOString().slice(0, 10)
  const path = await save({
    defaultPath: `devdy-settings-${stamp}.${FILE_EXT}`,
    filters: [{ name: 'Devdy backup', extensions: [FILE_EXT] }],
  })
  if (!path) return
  exporting.value = true
  try {
    await invoke<BackupInfo>('export_settings_backup', {
      path,
      password: includeSecrets.value ? exportPassword.value : null,
    })
    exportPassword.value = ''
    exportPasswordConfirm.value = ''
    toast.success(t('settings.backup.export.done'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    exporting.value = false
  }
}

// ---- Import ----
const importPath = ref<string | null>(null)
const importInfo = ref<BackupInfo | null>(null)
const importPassword = ref('')
const importing = ref(false)
const report = ref<ImportReport | null>(null)

const SECTION_KEYS = [
  'settings', 'skills', 'rules', 'skillGroups', 'ruleGroups', 'mcpServers', 'vpsServers',
  'githubAccounts', 'gitlabAccounts', 'awsAccounts', 'azureAccounts', 'googleAccounts',
  'claudeAccounts', 'notes', 'todos',
] as const

const infoRows = computed(() =>
  SECTION_KEYS
    .map(key => ({ key, count: importInfo.value?.counts[key] ?? 0 }))
    .filter(row => row.count > 0),
)

const reportRows = computed(() =>
  SECTION_KEYS
    .map(key => ({ key, c: report.value?.counts[key] }))
    .filter((row): row is { key: (typeof SECTION_KEYS)[number]; c: ImportCounts } => !!row.c),
)

function formatExportedAt(value: string) {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString()
}

async function chooseFile() {
  const selected = await open({
    multiple: false,
    filters: [{ name: 'Devdy backup', extensions: [FILE_EXT, 'json'] }],
  })
  if (!selected || Array.isArray(selected)) return
  report.value = null
  importPassword.value = ''
  try {
    importInfo.value = await invoke<BackupInfo>('inspect_settings_backup', { path: selected })
    importPath.value = selected
  } catch (e) {
    importInfo.value = null
    importPath.value = null
    toast.error(String(e))
  }
}

async function runImport() {
  if (!importPath.value || importing.value) return
  importing.value = true
  try {
    report.value = await invoke<ImportReport>('import_settings_backup', {
      path: importPath.value,
      password: importInfo.value?.hasSecrets && importPassword.value ? importPassword.value : null,
    })
    importPassword.value = ''
    importInfo.value = null
    importPath.value = null
    toast.success(t('settings.backup.import.done'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    importing.value = false
  }
}

function cancelImport() {
  importInfo.value = null
  importPath.value = null
  importPassword.value = ''
}

// Settings, language and every list store were loaded before the import: reload to pick it all up.
function reloadApp() {
  window.location.reload()
}
</script>

<template>
  <Card body-class="p-4 space-y-5">
    <template #header>
      <ArchiveRestore class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('settings.backup.title') }}</span>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('settings.backup.help') }}</p>

    <!-- Export -->
    <section class="space-y-3">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
        {{ t('settings.backup.export.title') }}
      </label>

      <div class="flex items-center justify-between gap-4 rounded-lg border border-border/60 bg-muted/20 px-3.5 py-3">
        <div class="min-w-0">
          <p class="text-sm font-medium text-foreground">{{ t('settings.backup.export.includeSecrets') }}</p>
          <p class="mt-0.5 text-[11px] text-muted-foreground">{{ t('settings.backup.export.includeSecretsHint') }}</p>
        </div>
        <button
          type="button"
          role="switch"
          :aria-checked="includeSecrets"
          :aria-label="t('settings.backup.export.includeSecrets')"
          class="relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
          :class="includeSecrets ? 'bg-primary' : 'bg-muted'"
          @click="includeSecrets = !includeSecrets"
        >
          <span
            class="inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform"
            :class="includeSecrets ? 'translate-x-6' : 'translate-x-1'"
          />
        </button>
      </div>

      <div v-if="includeSecrets" class="space-y-2">
        <Input
          v-model="exportPassword"
          type="password"
          autocomplete="new-password"
          :placeholder="t('settings.backup.export.password')"
        />
        <Input
          v-model="exportPasswordConfirm"
          type="password"
          autocomplete="new-password"
          :placeholder="t('settings.backup.export.passwordConfirm')"
        />
        <p class="flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
          <KeyRound class="mt-px h-3 w-3 shrink-0" :stroke-width="1.75" />
          {{ t('settings.backup.export.passwordWarning') }}
        </p>
        <p v-if="exportError && exportPassword" class="text-[11px] text-destructive">{{ exportError }}</p>
      </div>
      <p v-else class="text-[11px] text-muted-foreground">{{ t('settings.backup.export.noSecretsHint') }}</p>

      <Button size="sm" :disabled="exporting || !!exportError" @click="runExport">
        <component :is="exporting ? Loader2 : Download" class="h-3.5 w-3.5" :class="{ 'animate-spin': exporting }" :stroke-width="1.75" />
        {{ t('settings.backup.export.button') }}
      </Button>
    </section>

    <div class="border-t border-border/60" />

    <!-- Import -->
    <section class="space-y-3">
      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
        {{ t('settings.backup.import.title') }}
      </label>
      <p class="text-[11px] text-muted-foreground">{{ t('settings.backup.import.help') }}</p>

      <Button v-if="!importInfo" variant="outline" size="sm" @click="chooseFile">
        <FileUp class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('settings.backup.import.choose') }}
      </Button>

      <div v-else class="space-y-3 rounded-lg border border-border/60 bg-muted/20 p-3">
        <div class="text-[11px] text-muted-foreground space-y-0.5">
          <div class="truncate font-mono" :title="importPath ?? ''">{{ importPath }}</div>
          <div>
            {{ t('settings.backup.import.exportedAt', { at: formatExportedAt(importInfo.exportedAt), version: importInfo.appVersion || '?' }) }}
          </div>
        </div>
        <ul class="grid grid-cols-2 gap-x-4 gap-y-0.5 text-xs">
          <li v-for="row in infoRows" :key="row.key" class="flex justify-between gap-2">
            <span class="text-muted-foreground">{{ t(`settings.backup.sections.${row.key}`) }}</span>
            <span class="tabular-nums">{{ row.count }}</span>
          </li>
        </ul>

        <div v-if="importInfo.hasSecrets" class="space-y-1.5">
          <Input
            v-model="importPassword"
            type="password"
            autocomplete="off"
            :placeholder="t('settings.backup.import.password')"
          />
          <p class="text-[11px] text-muted-foreground">{{ t('settings.backup.import.passwordHint') }}</p>
        </div>
        <p v-else class="text-[11px] text-muted-foreground">{{ t('settings.backup.import.noSecrets') }}</p>

        <div class="flex items-center gap-2">
          <Button size="sm" :disabled="importing" @click="runImport">
            <component :is="importing ? Loader2 : Upload" class="h-3.5 w-3.5" :class="{ 'animate-spin': importing }" :stroke-width="1.75" />
            {{ t('settings.backup.import.button') }}
          </Button>
          <Button variant="ghost" size="sm" :disabled="importing" @click="cancelImport">
            {{ t('settings.backup.import.cancel') }}
          </Button>
        </div>
      </div>

      <!-- Result -->
      <div v-if="report" class="space-y-2 rounded-lg border border-border/60 p-3">
        <p class="text-xs font-medium">{{ t('settings.backup.report.title') }}</p>
        <table class="w-full text-xs">
          <thead class="text-[10px] uppercase tracking-wider text-muted-foreground">
            <tr>
              <th class="text-left font-medium" />
              <th class="text-right font-medium">{{ t('settings.backup.report.created') }}</th>
              <th class="text-right font-medium">{{ t('settings.backup.report.updated') }}</th>
              <th class="text-right font-medium">{{ t('settings.backup.report.unchanged') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in reportRows" :key="row.key">
              <td class="text-muted-foreground">{{ t(`settings.backup.sections.${row.key}`) }}</td>
              <td class="text-right tabular-nums">{{ row.c.created }}</td>
              <td class="text-right tabular-nums">{{ row.c.updated }}</td>
              <td class="text-right tabular-nums text-muted-foreground">{{ row.c.unchanged }}</td>
            </tr>
          </tbody>
        </table>
        <p class="text-[11px] text-muted-foreground">
          {{ report.secretsImported ? t('settings.backup.report.secretsImported') : t('settings.backup.report.secretsNotImported') }}
        </p>
        <p class="text-[11px] text-muted-foreground">{{ t('settings.backup.report.reloginHint') }}</p>
        <ul v-if="report.warnings.length" class="space-y-1 text-[11px] text-amber-600 dark:text-amber-400">
          <li v-for="(w, i) in report.warnings" :key="i" class="flex items-start gap-1.5">
            <AlertTriangle class="mt-px h-3 w-3 shrink-0" :stroke-width="1.75" />
            <span class="break-all">{{ w }}</span>
          </li>
        </ul>
        <Button variant="outline" size="xs" @click="reloadApp">
          <RotateCcw class="h-3 w-3" :stroke-width="1.75" />
          {{ t('settings.backup.report.reload') }}
        </Button>
      </div>
    </section>
  </Card>
</template>
