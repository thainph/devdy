<script setup lang="ts">
/**
 * Link entry (Phase 4): auto-parse the session link from the URL hash (a QR is a
 * clickable controller URL) and, on success, emit `link` WITHOUT connecting —
 * the parent then connects and waits for the Host OTP gate. A parse failure just
 * leaves the manual paste box for the user.
 */
import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { KeyRound, Link2, Loader2, ShieldCheck } from 'lucide-vue-next'
import { Button, Input, Textarea } from '@/components/ui'
import { parseSessionLink, SessionLinkParseError, type SessionLinkPayload } from '../protocol'
import { LoginError, loginWithPassword, type LoginGrant } from '../login'

const { t } = useI18n()

const emit = defineEmits<{ link: [payload: SessionLinkPayload]; granted: [grant: LoginGrant] }>()

// Password login: for when the link is lost. The owner's master password alone
// reaches the Host through its login slot.
const password = ref('')
const loggingIn = ref(false)
const loginError = ref<string | null>(null)

async function submitPassword(): Promise<void> {
  if (loggingIn.value) return
  if (!password.value) {
    loginError.value = t('controller.link.passwordFirst')
    return
  }
  loggingIn.value = true
  loginError.value = null
  try {
    const grant = await loginWithPassword(password.value)
    password.value = ''
    emit('granted', grant)
  } catch (e) {
    const code = e instanceof LoginError ? e.code : 'network'
    loginError.value =
      code === 'wrong_password'
        ? t('controller.link.loginWrongPassword')
        : code === 'host_offline'
          ? t('controller.link.loginHostOffline')
          : t('controller.link.loginNetwork')
  } finally {
    loggingIn.value = false
  }
}

const manual = ref('')
const error = ref<string | null>(null)
const autoFromUrl = ref(false)

function tryLink(raw: string): void {
  error.value = null
  try {
    emit('link', parseSessionLink(raw))
  } catch (e) {
    error.value = e instanceof SessionLinkParseError ? e.message : t('controller.link.couldNotRead')
  }
}

function submitManual(): void {
  if (!manual.value.trim()) {
    error.value = t('controller.link.pasteFirst')
    return
  }
  tryLink(manual.value)
}

// A QR that is a controller deep-link lands here with the params in the hash.
onMounted(() => {
  const hash = window.location.hash.replace(/^#/, '')
  if (!hash) return
  try {
    const payload = parseSessionLink(
      hash.includes('=') || hash.startsWith('{') ? hash : decodeURIComponent(hash),
    )
    autoFromUrl.value = true
    emit('link', payload)
  } catch {
    // Not a valid link in the URL — show the manual entry.
  }
})
</script>

<template>
  <div class="min-h-dvh flex items-center justify-center px-4 py-10 bg-background text-foreground">
    <div class="w-full max-w-md space-y-6">
      <div class="text-center space-y-2">
        <div class="inline-flex h-12 w-12 items-center justify-center rounded-xl bg-primary/15 text-primary">
          <ShieldCheck class="h-6 w-6" :stroke-width="1.75" />
        </div>
        <h1 class="text-lg font-semibold">{{ t('controller.link.title') }}</h1>
        <p class="text-sm text-foreground/60">
          {{ t('controller.link.subtitle') }}
        </p>
      </div>

      <div v-if="autoFromUrl" class="text-center text-sm text-foreground/60">
        {{ t('controller.link.connectingFromScan') }}
      </div>

      <div class="space-y-3">
        <label class="block text-xs font-medium uppercase tracking-wider text-foreground/50">
          {{ t('controller.link.controllerLink') }}
        </label>
        <Textarea
          v-model="manual"
          :rows="4"
          :placeholder="t('controller.link.linkPlaceholder')"
          class="font-mono text-xs"
          @keydown.meta.enter="submitManual"
          @keydown.ctrl.enter="submitManual"
        />
        <Button variant="primary" class="w-full" @click="submitManual">
          <Link2 class="h-4 w-4" :stroke-width="2" /> {{ t('controller.link.continue') }}
        </Button>
        <p v-if="error" class="text-sm text-destructive text-center">{{ error }}</p>
      </div>

      <div class="space-y-3 border-t border-border pt-6">
        <label class="block text-xs font-medium uppercase tracking-wider text-foreground/50">
          {{ t('controller.link.passwordLabel') }}
        </label>
        <form class="space-y-3" @submit.prevent="submitPassword">
          <Input
            v-model="password"
            type="password"
            autocomplete="current-password"
            :placeholder="t('controller.link.passwordPlaceholder')"
            :disabled="loggingIn"
          />
          <Button type="submit" variant="outline" class="w-full" :disabled="loggingIn">
            <Loader2 v-if="loggingIn" class="h-4 w-4 animate-spin" :stroke-width="2" />
            <KeyRound v-else class="h-4 w-4" :stroke-width="2" />
            {{ loggingIn ? t('controller.link.passwordWorking') : t('controller.link.passwordLogin') }}
          </Button>
        </form>
        <p v-if="loginError" class="text-sm text-destructive text-center">{{ loginError }}</p>
        <p class="text-[11px] leading-relaxed text-foreground/40 text-center">
          {{ t('controller.link.passwordHint') }}
        </p>
      </div>

      <p class="text-[11px] leading-relaxed text-foreground/40 text-center">
        {{ t('controller.link.e2eNote') }}
      </p>
    </div>
  </div>
</template>
