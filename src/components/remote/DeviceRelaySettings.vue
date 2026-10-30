<script setup lang="ts">
// Settings → Devices & Sync, top card: what Remote Control and Sync share — the relay
// (URL + auth token), this device's name and its master password. Another device enters
// that name + password both to drive this one and to join its sync group, and the sync
// group lists this device under the same name.
import { onMounted, ref, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button, Input, Card } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { DEVICE_ALIAS_RE } from '@/lib/remoteLogin'
import { KeyRound } from 'lucide-vue-next'

const store = useRemoteControlStore()
const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()

const relayUrl = ref('')
const authToken = ref('')
const savingConfig = ref(false)
const masterPassword = ref('')
const savingMasterPw = ref(false)
const deviceAlias = ref('')

const status = computed(() => store.status)

function fill() {
  relayUrl.value = store.status?.relay_url ?? ''
  deviceAlias.value = store.status?.device_alias ?? ''
}

onMounted(async () => {
  try {
    await store.refreshStatus()
    fill()
  } catch (e) {
    toast.error(String(e))
  }
})

async function saveConfig() {
  const url = relayUrl.value.trim()
  if (!url) {
    toast.error(t('remote.settings.toast.relayRequired'))
    return
  }
  const alias = deviceAlias.value.trim().toLowerCase()
  if (!DEVICE_ALIAS_RE.test(alias)) {
    toast.error(t('remote.settings.toast.deviceNameInvalid'))
    return
  }
  savingConfig.value = true
  try {
    await store.setConfig(url, authToken.value)
    authToken.value = '' // never keep the token around in the field
    if (alias !== (store.status?.device_alias ?? '')) await store.setDeviceAlias(alias)
    deviceAlias.value = alias
    // A non-empty field sets/updates the master password; blank leaves it
    // untouched (same "blank keeps current" convention as the auth token).
    // Clearing is done explicitly via the Clear link.
    if (masterPassword.value.trim()) {
      await store.setMasterPassword(masterPassword.value)
      masterPassword.value = ''
    }
    toast.success(t('remote.settings.toast.settingsSaved'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    savingConfig.value = false
  }
}

async function clearMasterPassword() {
  if (savingMasterPw.value) return
  const ok = await confirm({
    title: t('remote.settings.confirmClearMaster.title'),
    message: t('remote.settings.confirmClearMaster.message'),
    confirmLabel: t('remote.settings.confirmClearMaster.confirmLabel'),
    variant: 'destructive',
  })
  if (!ok) return
  savingMasterPw.value = true
  try {
    await store.setMasterPassword('')
    masterPassword.value = ''
    toast.success(t('remote.settings.toast.masterCleared'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    savingMasterPw.value = false
  }
}
</script>

<template>
  <Card>
    <template #header>
      <KeyRound class="h-4 w-4 text-primary" :stroke-width="1.75" />
      <h3 class="text-sm font-semibold flex-1">{{ t('remote.settings.identity.title') }}</h3>
    </template>
    <div class="p-4 space-y-4">
      <p class="text-xs text-muted-foreground">{{ t('remote.settings.identity.intro') }}</p>

      <div class="space-y-1.5">
        <label class="text-xs font-medium text-muted-foreground">{{ t('remote.settings.relayUrl') }}</label>
        <Input
          v-model="relayUrl"
          :placeholder="t('remote.settings.relayUrlPlaceholder')"
          :disabled="savingConfig"
        />
      </div>

      <div class="space-y-1.5">
        <label class="text-xs font-medium text-muted-foreground">
          {{ t('remote.settings.authToken') }}
          <span v-if="status?.has_auth_token" class="text-emerald-500">{{ t('remote.settings.saved') }}</span>
        </label>
        <Input
          v-model="authToken"
          type="password"
          :placeholder="t('remote.settings.authTokenPlaceholder')"
          :disabled="savingConfig"
        />
        <p class="text-[11px] text-muted-foreground/70">
          {{ t('remote.settings.authTokenHint') }}
        </p>
      </div>

      <!-- Device name: picks this host's password-login code, so several
           Devdy hosts can share one relay (and one master password). -->
      <div class="space-y-1.5">
        <label class="text-xs font-medium text-muted-foreground">{{ t('remote.settings.deviceName') }}</label>
        <Input
          v-model="deviceAlias"
          autocomplete="off"
          spellcheck="false"
          :placeholder="t('remote.settings.deviceNamePlaceholder')"
          :disabled="savingConfig"
          class="font-mono"
          @keyup.enter="saveConfig"
        />
        <p class="text-[11px] text-muted-foreground/70">
          {{ t('remote.settings.deviceNameHint') }}
        </p>
      </div>

      <!-- Master password: an optional stable code that replaces the per-join
           OTP for a single owner (falls back to OTP when unset). -->
      <div class="space-y-1.5">
        <label class="flex items-center gap-2 text-xs font-medium text-muted-foreground">
          <span>
            {{ t('remote.settings.masterPassword') }}
            <span v-if="status?.has_master_password" class="text-emerald-500">{{ t('remote.settings.set') }}</span>
          </span>
          <button
            v-if="status?.has_master_password"
            type="button"
            :disabled="savingMasterPw"
            class="ml-auto font-normal text-[11px] text-muted-foreground/80 hover:text-destructive transition-colors disabled:opacity-50"
            @click="clearMasterPassword"
          >
            {{ t('common.clear') }}
          </button>
        </label>
        <Input
          v-model="masterPassword"
          type="password"
          :placeholder="status?.has_master_password
            ? t('remote.settings.masterPasswordReplace')
            : t('remote.settings.masterPasswordSet')"
          :disabled="savingConfig || savingMasterPw"
          @keyup.enter="saveConfig"
        />
        <p class="text-[11px] text-muted-foreground/70">
          {{ t('remote.settings.masterPasswordHint') }}
        </p>
      </div>

      <div class="flex items-center gap-2 pt-1">
        <Button variant="outline" :disabled="savingConfig" @click="saveConfig">
          {{ t('remote.settings.saveSettings') }}
        </Button>
        <span v-if="status?.device_id" class="ml-auto text-[11px] text-muted-foreground/60 font-mono">
          {{ t('remote.settings.hostLabel', { id: status.device_id.slice(0, 8) }) }}
        </span>
      </div>
    </div>
  </Card>
</template>
