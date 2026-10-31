<script setup lang="ts">
// Settings → "Azure Accounts": each account is an isolated `az login` managed by
// Devdy (its own Azure CLI config dir), so several tenants/identities can coexist.
// Mirrors the AWS section: validate, sign in again, edit (label, tags, pinned
// subscription) and delete. Adding an account opens the browser sign-in and the
// backend call blocks until the user finishes it.
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlertTriangle, CheckCircle2, CloudCog, Loader2, LogIn, Pencil, Plus, ShieldAlert, Trash2,
} from 'lucide-vue-next'
import { AppSelect, Button, Card, Input } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import {
  useAzureAccountsStore, isAzureLoginRequired,
  type AzureAccount, type AzureSubscription, type AzureValidation,
} from '@/stores/azureAccounts'

const { t } = useI18n()
const { confirm } = useConfirm()
const { toast } = useToast()
const store = useAzureAccountsStore()

// --- Account list ---
const validations = ref<Record<string, AzureValidation>>({})
const accountError = ref<Record<string, string>>({})
// Busy state is per account: a 5-minute `az login` on one account must keep
// that account's actions disabled while another account is validated/edited.
const busy = ref<Record<string, boolean>>({})
// Accounts currently running the browser sign-in (subset of `busy`).
const loggingIn = ref<Record<string, boolean>>({})
// Accounts whose last call failed because their az login needs refreshing.
const loginNeeded = ref<Record<string, boolean>>({})

function needsLogin(acc: AzureAccount): boolean {
  return !acc.logged_in || !!loginNeeded.value[acc.id]
}

function fmtTime(iso: string | null | undefined): string {
  if (!iso) return ''
  const d = new Date(iso)
  return isNaN(d.getTime()) ? '' : d.toLocaleString()
}

// Validation without touching the busy flag, so the login flow can re-validate
// while it still holds the account.
async function runValidate(id: string) {
  accountError.value[id] = ''
  loginNeeded.value[id] = false
  delete validations.value[id]
  try {
    validations.value[id] = await store.validate(id)
  } catch (e) {
    if (isAzureLoginRequired(e)) loginNeeded.value[id] = true
    else accountError.value[id] = String(e)
  }
}

async function handleValidate(id: string) {
  if (busy.value[id]) return
  busy.value[id] = true
  try {
    await runValidate(id)
  } finally {
    busy.value[id] = false
  }
}

async function handleLogin(id: string) {
  if (busy.value[id]) return
  busy.value[id] = true
  loggingIn.value[id] = true
  accountError.value[id] = ''
  try {
    await store.login(id)
    loginNeeded.value[id] = false
    loggingIn.value[id] = false
    // Re-validate straight away so a successful login turns the row green.
    await runValidate(id)
  } catch (e) {
    accountError.value[id] = String(e)
  } finally {
    loggingIn.value[id] = false
    busy.value[id] = false
  }
}

async function handleDelete(id: string) {
  if (busy.value[id]) return
  if (!(await confirm({
    title: t('settings.azure.deleteTitle'),
    message: t('settings.azure.deleteMessage'),
    confirmLabel: t('common.delete'),
  }))) return
  busy.value[id] = true
  try {
    await store.remove(id)
    toast.success(t('settings.github.accountDeleted'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    delete busy.value[id]
  }
}

// --- Edit (label, tags, pinned subscription) ---
const editing = ref<string | null>(null)
const editLabel = ref<Record<string, string>>({})
const editTags = ref<Record<string, string>>({})
const editSubscription = ref<Record<string, string>>({})
const subscriptions = ref<Record<string, AzureSubscription[]>>({})
const subscriptionsLoading = ref<string | null>(null)
// Listing failed → fall back to a free-text subscription field.
const subscriptionsFailed = ref<Record<string, boolean>>({})

function subscriptionOptions(id: string) {
  return (subscriptions.value[id] ?? []).map((s) => ({
    value: s.id,
    label: s.is_default ? `${s.name} (${t('settings.azure.subscriptionDefault')})` : s.name,
    description: s.id,
  }))
}

async function loadSubscriptions(id: string) {
  subscriptionsLoading.value = id
  subscriptionsFailed.value[id] = false
  try {
    subscriptions.value[id] = await store.listSubscriptions(id)
  } catch (e) {
    subscriptionsFailed.value[id] = true
    if (isAzureLoginRequired(e)) loginNeeded.value[id] = true
    else accountError.value[id] = String(e)
  } finally {
    if (subscriptionsLoading.value === id) subscriptionsLoading.value = null
  }
}

function startEdit(acc: AzureAccount) {
  editing.value = acc.id
  accountError.value[acc.id] = ''
  editLabel.value[acc.id] = acc.label
  editTags.value[acc.id] = acc.tags ?? ''
  editSubscription.value[acc.id] = acc.subscription_id ?? ''
  loadSubscriptions(acc.id)
}

async function handleSaveEdit(id: string) {
  if (busy.value[id]) return
  busy.value[id] = true
  accountError.value[id] = ''
  try {
    await store.update(id, {
      label: editLabel.value[id]?.trim() || '',
      subscription: editSubscription.value[id]?.trim() || undefined,
      tags: editTags.value[id]?.trim(),
    })
    editing.value = null
    // The pinned subscription may have changed — drop the stale validation.
    delete validations.value[id]
    toast.success(t('settings.github.accountUpdated'))
  } catch (e) {
    if (isAzureLoginRequired(e)) loginNeeded.value[id] = true
    accountError.value[id] = String(e)
  } finally {
    busy.value[id] = false
  }
}

// --- Add account (browser `az login`, blocks until done) ---
const newLabel = ref('')
const newTenant = ref('')
const newSubscription = ref('')
const newTags = ref('')
const adding = ref(false)
const addError = ref<string | null>(null)

async function handleAdd() {
  const label = newLabel.value.trim()
  if (!label || adding.value) return
  adding.value = true
  addError.value = null
  try {
    await store.create({
      label,
      tenant: newTenant.value.trim() || undefined,
      subscription: newSubscription.value.trim() || undefined,
      tags: newTags.value.trim() || undefined,
    })
    newLabel.value = ''
    newTenant.value = ''
    newSubscription.value = ''
    newTags.value = ''
    toast.success(t('settings.azure.accountAdded'))
  } catch (e) {
    addError.value = isAzureLoginRequired(e) ? t('settings.azure.addLoginFailed') : String(e)
  } finally {
    adding.value = false
  }
}

onMounted(() => {
  store.fetch()
})
</script>

<template>
  <Card body-class="p-4 space-y-4">
    <template #header>
      <CloudCog class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('settings.azure.title') }}</span>
    </template>
    <p class="text-[11px] text-muted-foreground leading-relaxed">
      {{ t('settings.azure.intro') }}
    </p>

    <div class="p-2.5 bg-amber-500/10 border border-amber-500/20 rounded-md text-[11px] leading-relaxed flex gap-2">
      <ShieldAlert class="h-3.5 w-3.5 text-amber-500 shrink-0 mt-0.5" :stroke-width="1.75" />
      <span class="text-muted-foreground">
        {{ t('settings.azure.isolationWarning') }}
      </span>
    </div>

    <p v-if="store.error" class="text-[11px] text-destructive">{{ store.error }}</p>

    <!-- Account list -->
    <div v-if="store.accounts.length" class="space-y-2">
      <div
        v-for="acc in store.accounts"
        :key="acc.id"
        class="border border-border rounded-md p-3 space-y-2"
      >
        <template v-if="editing !== acc.id">
          <div class="flex items-center justify-between gap-2">
            <div class="min-w-0">
              <div class="text-sm font-medium truncate">{{ acc.label }}</div>
              <div class="text-[11px] text-muted-foreground truncate">
                <span v-if="acc.subscription_name">{{ acc.subscription_name }}</span>
                <span v-else-if="acc.subscription_id" class="font-mono">{{ acc.subscription_id }}</span>
                <span v-else>{{ t('settings.azure.noSubscription') }}</span>
                <span v-if="acc.tenant_id || acc.tenant"> · <span class="font-mono">{{ acc.tenant_id ?? acc.tenant }}</span></span>
              </div>
              <div class="text-[11px] text-muted-foreground truncate">
                <span v-if="acc.user_name">{{ acc.user_name }} · </span>
                <span>{{ acc.last_validated_at
                  ? t('settings.azure.lastValidated', { time: fmtTime(acc.last_validated_at) })
                  : t('settings.azure.neverValidated') }}</span>
              </div>
            </div>
            <div class="flex items-center gap-1 shrink-0">
              <Button
                variant="outline"
                size="xs"
                :disabled="busy[acc.id]"
                @click="handleValidate(acc.id)"
              >
                {{ busy[acc.id] && !loggingIn[acc.id] ? '…' : t('settings.azure.validate') }}
              </Button>
              <Button
                v-if="loggingIn[acc.id] || needsLogin(acc)"
                variant="outline"
                size="xs"
                :disabled="busy[acc.id]"
                @click="handleLogin(acc.id)"
              >
                <Loader2 v-if="loggingIn[acc.id]" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
                <LogIn v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
                {{ t('settings.azure.login') }}
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                :title="t('common.edit')"
                :disabled="busy[acc.id]"
                @click="startEdit(acc)"
              >
                <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
              <Button
                variant="destructive-ghost"
                size="icon-sm"
                :title="t('common.delete')"
                :disabled="busy[acc.id]"
                @click="handleDelete(acc.id)"
              >
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
            </div>
          </div>
          <div
            v-if="validations[acc.id]"
            class="p-2 bg-emerald-500/10 border border-emerald-500/20 rounded-md text-[11px]"
          >
            <div class="flex items-center gap-1.5 text-emerald-500 font-medium">
              <CheckCircle2 class="h-3 w-3" :stroke-width="2" />
              {{ t('settings.azure.valid') }}
            </div>
            <p class="text-muted-foreground mt-1 truncate">
              {{ validations[acc.id].subscription_name }}
              <span class="font-mono"> · {{ validations[acc.id].subscription_id }}</span>
            </p>
            <p class="text-muted-foreground mt-1 truncate">
              {{ validations[acc.id].user_name }}
              <span class="font-mono"> · {{ validations[acc.id].tenant_id }}</span>
            </p>
          </div>
          <div
            v-if="loggingIn[acc.id] || needsLogin(acc)"
            class="p-2 bg-amber-500/10 border border-amber-500/20 rounded-md text-[11px] text-amber-500 flex items-start gap-1.5"
          >
            <Loader2 v-if="loggingIn[acc.id]" class="h-3 w-3 shrink-0 mt-0.5 animate-spin" :stroke-width="1.75" />
            <AlertTriangle v-else class="h-3 w-3 shrink-0 mt-0.5" :stroke-width="1.75" />
            <span>{{ loggingIn[acc.id] ? t('settings.azure.loggingIn') : t('settings.azure.loginExpired') }}</span>
          </div>
        </template>

        <template v-else>
          <Input v-model="editLabel[acc.id]" size="sm" :placeholder="t('settings.azure.labelPlaceholder')" />
          <div class="space-y-1.5">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('settings.azure.subscriptionLabel') }}</label>
            <p v-if="subscriptionsLoading === acc.id" class="text-[11px] text-muted-foreground">{{ t('settings.azure.subscriptionsLoading') }}</p>
            <Input
              v-else-if="subscriptionsFailed[acc.id]"
              v-model="editSubscription[acc.id]"
              size="sm"
              :placeholder="t('settings.azure.subscriptionPlaceholder')"
              class="font-mono"
            />
            <AppSelect
              v-else
              v-model="editSubscription[acc.id]"
              size="sm"
              :options="subscriptionOptions(acc.id)"
              :placeholder="t('settings.azure.subscriptionSelectPlaceholder')"
            />
          </div>
          <Input v-model="editTags[acc.id]" size="sm" :placeholder="t('settings.azure.tagsPlaceholder')" />
          <div class="flex items-center gap-2">
            <Button
              :disabled="!editLabel[acc.id]?.trim() || busy[acc.id]"
              @click="handleSaveEdit(acc.id)"
            >
              {{ busy[acc.id] ? t('settings.azure.saving') : t('common.save') }}
            </Button>
            <Button variant="outline" @click="editing = null">{{ t('common.cancel') }}</Button>
          </div>
        </template>

        <p v-if="accountError[acc.id]" class="text-[11px] text-destructive">{{ accountError[acc.id] }}</p>
      </div>
    </div>

    <!-- Add account: browser az login into an isolated config dir -->
    <div class="border-t border-border/60 pt-3 space-y-2">
      <div class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('settings.azure.addAccount') }}</div>
      <p class="text-[11px] text-muted-foreground leading-relaxed">{{ t('settings.azure.addHint') }}</p>
      <Input
        v-model="newLabel"
        size="sm"
        :disabled="adding"
        :placeholder="t('settings.azure.addLabelPlaceholder')"
        @keyup.enter="handleAdd"
      />
      <Input v-model="newTenant" size="sm" :disabled="adding" :placeholder="t('settings.azure.tenantPlaceholder')" class="font-mono" />
      <Input v-model="newSubscription" size="sm" :disabled="adding" :placeholder="t('settings.azure.subscriptionPlaceholder')" class="font-mono" />
      <Input v-model="newTags" size="sm" :disabled="adding" :placeholder="t('settings.azure.tagsPlaceholder')" />
      <div
        v-if="adding"
        class="p-2 bg-amber-500/10 border border-amber-500/20 rounded-md text-[11px] text-amber-500 flex items-start gap-1.5"
      >
        <Loader2 class="h-3 w-3 shrink-0 mt-0.5 animate-spin" :stroke-width="1.75" />
        <span>{{ t('settings.azure.addWaiting') }}</span>
      </div>
      <div class="flex justify-end">
        <Button :disabled="!newLabel.trim() || adding" @click="handleAdd">
          <Loader2 v-if="adding" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
          <Plus v-else class="h-3.5 w-3.5" :stroke-width="2" />
          {{ adding ? t('settings.azure.loggingIn') : t('common.add') }}
        </Button>
      </div>
      <p v-if="addError" class="text-[11px] text-destructive">{{ addError }}</p>
    </div>
  </Card>
</template>
