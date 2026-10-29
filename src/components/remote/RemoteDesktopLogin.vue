<script setup lang="ts">
/**
 * Sign-in screen of the remote-desktop window (`?remote=1`) when no session is
 * stored yet: the controlled Devdy's device name + its master password, over
 * the relay THIS machine is configured with. On success the granted link +
 * token are stored and the window reloads into the remote app.
 */
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-real/core'
import { KeyRound, Loader2, MonitorSmartphone } from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import { LoginError, loginWithPassword } from '@/controller/login'
import { DEVICE_ALIAS_RE, relayWsUrl } from '@/lib/remoteLogin'
import { saveRemoteDesktopSession } from '@/lib/remoteDesktopSession'

const { t } = useI18n()

/** Last device signed in to, prefilled next time. */
const LAST_DEVICE_KEY = 'devdy.remoteDesktop.lastDevice'

const device = ref(readLastDevice())
const password = ref('')
const relayUrl = ref<string | null>(null)
const loadingRelay = ref(true)
const loggingIn = ref(false)
const error = ref<string | null>(null)

function readLastDevice(): string {
  try {
    return localStorage.getItem(LAST_DEVICE_KEY) ?? ''
  } catch {
    return ''
  }
}

onMounted(async () => {
  // This machine's own backend (the shim is not started on this screen).
  try {
    const status = await invoke<{ relay_url: string }>('remote_status')
    relayUrl.value = relayWsUrl(status.relay_url ?? '')
  } catch {
    relayUrl.value = null
  } finally {
    loadingRelay.value = false
  }
})

async function submit(): Promise<void> {
  if (loggingIn.value) return
  const relay = relayUrl.value
  if (!relay) {
    error.value = t('remote.desktop.login.noRelay')
    return
  }
  const name = device.value.trim().toLowerCase()
  if (!DEVICE_ALIAS_RE.test(name)) {
    error.value = t('remote.desktop.login.deviceInvalid')
    return
  }
  if (!password.value) {
    error.value = t('remote.desktop.login.passwordFirst')
    return
  }
  loggingIn.value = true
  error.value = null
  try {
    const grant = await loginWithPassword(password.value, { relayUrl: relay, device: name })
    password.value = ''
    saveRemoteDesktopSession({ link: grant.link, token: grant.token, device: name })
    try {
      localStorage.setItem(LAST_DEVICE_KEY, name)
    } catch {
      // not remembering the name is fine
    }
    location.reload()
  } catch (e) {
    const code = e instanceof LoginError ? e.code : 'network'
    error.value =
      code === 'wrong_password'
        ? t('remote.desktop.login.wrongPassword')
        : code === 'host_offline'
          ? t('remote.desktop.login.hostOffline')
          : t('remote.desktop.login.network')
    loggingIn.value = false
  }
}
</script>

<template>
  <div class="min-h-dvh flex items-center justify-center px-4 py-10 bg-background text-foreground">
    <div class="w-full max-w-sm space-y-6">
      <div class="text-center space-y-2">
        <div class="inline-flex h-12 w-12 items-center justify-center rounded-xl bg-primary/15 text-primary">
          <MonitorSmartphone class="h-6 w-6" :stroke-width="1.75" />
        </div>
        <h1 class="text-lg font-semibold">{{ t('remote.desktop.login.title') }}</h1>
        <p class="text-sm text-foreground/60">{{ t('remote.desktop.login.subtitle') }}</p>
      </div>

      <form class="space-y-3" @submit.prevent="submit">
        <div class="space-y-1.5">
          <label class="text-xs font-medium text-muted-foreground">{{ t('remote.desktop.login.device') }}</label>
          <Input
            v-model="device"
            autocomplete="off"
            spellcheck="false"
            class="font-mono"
            :placeholder="t('remote.desktop.login.devicePlaceholder')"
            :disabled="loggingIn"
          />
        </div>
        <div class="space-y-1.5">
          <label class="text-xs font-medium text-muted-foreground">{{ t('remote.desktop.login.password') }}</label>
          <Input
            v-model="password"
            type="password"
            autocomplete="current-password"
            :placeholder="t('remote.desktop.login.passwordPlaceholder')"
            :disabled="loggingIn"
          />
        </div>
        <Button type="submit" variant="primary" class="w-full" :disabled="loggingIn || loadingRelay">
          <Loader2 v-if="loggingIn" class="h-4 w-4 animate-spin" :stroke-width="2" />
          <KeyRound v-else class="h-4 w-4" :stroke-width="2" />
          {{ loggingIn ? t('remote.desktop.login.working') : t('remote.desktop.login.submit') }}
        </Button>
      </form>

      <p v-if="error" class="text-sm text-destructive text-center">{{ error }}</p>
      <p class="text-[11px] leading-relaxed text-foreground/40 text-center">
        <template v-if="relayUrl">{{ t('remote.desktop.login.relayVia', { url: relayUrl }) }}</template>
        <template v-else-if="!loadingRelay">{{ t('remote.desktop.login.noRelay') }}</template>
      </p>
    </div>
  </div>
</template>
