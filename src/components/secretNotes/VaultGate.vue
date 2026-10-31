<script setup lang="ts">
// What the Secret notes window shows while the notes are out of reach: loading,
// "create the vault" (no vault yet), "unlock" (password or recovery key) and,
// right after setup or a password change, the recovery key (shown once, until
// acknowledged). Centred like the windows' empty states: an icon tile, a title,
// one line of help, then the form.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { KeyRound, Loader2, LockKeyhole, ShieldCheck } from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import {
  passwordProblem,
  vaultError,
  vaultSetup,
  vaultUnlock,
  vaultUnlockRecovery,
  type VaultStatus,
} from '@/lib/secretVault'

defineProps<{
  status: VaultStatus | null
  loadError: string
  /** Shown once after setup / a password change, until acknowledged. */
  recoveryKey: string | null
}>()

const emit = defineEmits<{
  /** The vault was created: show this recovery key. */
  created: [recoveryKey: string]
  /** Unlocked; `viaRecovery` = with the recovery key (a new password is due). */
  unlocked: [viaRecovery: boolean]
  ackRecovery: []
}>()

const { t } = useI18n()
const { toast } = useToast()

function errText(e: unknown): string {
  const { code, wait } = vaultError(e)
  return code ? t(`secretNotes.errors.${code}`, { n: wait ?? 0 }) : String(e)
}

// ── Setup ────────────────────────────────────────────────────────────────────
const setupPw = ref('')
const setupPw2 = ref('')
const settingUp = ref(false)
const setupProblem = computed(() => (setupPw.value ? passwordProblem(setupPw.value, setupPw2.value) : null))

async function setup() {
  if (settingUp.value || passwordProblem(setupPw.value, setupPw2.value)) return
  settingUp.value = true
  try {
    const key = await vaultSetup(setupPw.value)
    setupPw.value = ''
    setupPw2.value = ''
    emit('created', key)
  } catch (e) {
    toast.error(errText(e))
  } finally {
    settingUp.value = false
  }
}

// ── Unlock ───────────────────────────────────────────────────────────────────
const unlockPw = ref('')
const useRecovery = ref(false)
const recoveryInput = ref('')
const unlocking = ref(false)
const unlockError = ref('')

async function unlock() {
  if (unlocking.value) return
  unlocking.value = true
  unlockError.value = ''
  const viaRecovery = useRecovery.value
  try {
    if (viaRecovery) await vaultUnlockRecovery(recoveryInput.value)
    else await vaultUnlock(unlockPw.value)
    unlockPw.value = ''
    recoveryInput.value = ''
    useRecovery.value = false
    emit('unlocked', viaRecovery)
  } catch (e) {
    unlockError.value = errText(e)
  } finally {
    unlocking.value = false
  }
}

function toggleRecovery() {
  useRecovery.value = !useRecovery.value
  unlockError.value = ''
}
</script>

<template>
  <div class="flex min-w-0 flex-1 items-center justify-center p-6">
    <!-- Loading / error -->
    <div v-if="!status" class="text-center text-xs">
      <p v-if="loadError" class="text-destructive">{{ loadError }}</p>
      <Loader2 v-else class="mx-auto h-4 w-4 animate-spin text-muted-foreground" :stroke-width="1.75" />
    </div>

    <!-- Recovery key, once (only while unlocked: it waits out a lock) -->
    <div v-else-if="recoveryKey && status.unlocked" class="w-full max-w-md text-center">
      <div class="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
        <KeyRound class="h-5 w-5 text-primary" :stroke-width="1.5" />
      </div>
      <h2 class="text-sm font-semibold">{{ t('secretNotes.recovery.title') }}</h2>
      <p class="mt-1 text-xs text-muted-foreground">{{ t('secretNotes.recovery.help') }}</p>
      <p
        class="mt-4 select-all break-all rounded-md border border-border/60 bg-muted/40 px-3 py-2.5 font-mono text-sm tracking-wide"
      >
        {{ recoveryKey }}
      </p>
      <p class="mt-2 text-[11px] text-amber-600 dark:text-amber-400">{{ t('secretNotes.recovery.warning') }}</p>
      <Button size="sm" class="mt-4" @click="emit('ackRecovery')">{{ t('secretNotes.recovery.done') }}</Button>
    </div>

    <!-- No vault yet -->
    <form v-else-if="!status.exists" class="w-full max-w-sm text-center" @submit.prevent="setup">
      <div class="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
        <ShieldCheck class="h-5 w-5 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <h2 class="text-sm font-semibold">{{ t('secretNotes.setup.title') }}</h2>
      <p class="mt-1 text-xs text-muted-foreground">{{ t('secretNotes.setup.help') }}</p>
      <p v-if="status.pending_count" class="mt-2 text-[11px] text-amber-600 dark:text-amber-400">
        {{ t('secretNotes.pending', { n: status.pending_count }) }}
      </p>
      <div class="mt-4 space-y-2 text-left">
        <Input v-model="setupPw" type="password" autocomplete="new-password" :placeholder="t('secretNotes.setup.password')" />
        <Input v-model="setupPw2" type="password" autocomplete="new-password" :placeholder="t('secretNotes.setup.confirm')" />
        <p v-if="setupProblem" class="text-[11px] text-destructive">{{ t(`secretNotes.password.${setupProblem}`) }}</p>
      </div>
      <Button type="submit" size="sm" class="mt-3 w-full" :disabled="settingUp || !!passwordProblem(setupPw, setupPw2)">
        <component
          :is="settingUp ? Loader2 : ShieldCheck"
          class="h-3.5 w-3.5"
          :class="{ 'animate-spin': settingUp }"
          :stroke-width="1.75"
        />
        {{ t('secretNotes.setup.button') }}
      </Button>
    </form>

    <!-- Locked -->
    <form v-else class="w-full max-w-sm text-center" @submit.prevent="unlock">
      <div class="mx-auto mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
        <LockKeyhole class="h-5 w-5 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <h2 class="text-sm font-semibold">{{ t('secretNotes.unlock.title') }}</h2>
      <p class="mt-1 text-xs text-muted-foreground">
        {{ t('secretNotes.unlock.help', { n: status.note_count }) }}
      </p>
      <div class="mt-4 space-y-2 text-left">
        <Input
          v-if="!useRecovery"
          v-model="unlockPw"
          type="password"
          autocomplete="current-password"
          autofocus
          :placeholder="t('secretNotes.unlock.password')"
        />
        <Input
          v-else
          v-model="recoveryInput"
          autocomplete="off"
          spellcheck="false"
          class="font-mono"
          :placeholder="t('secretNotes.unlock.recovery')"
        />
        <p v-if="unlockError" class="text-[11px] text-destructive">{{ unlockError }}</p>
      </div>
      <Button
        type="submit"
        size="sm"
        class="mt-3 w-full"
        :disabled="unlocking || (useRecovery ? !recoveryInput.trim() : !unlockPw)"
      >
        <component
          :is="unlocking ? Loader2 : LockKeyhole"
          class="h-3.5 w-3.5"
          :class="{ 'animate-spin': unlocking }"
          :stroke-width="1.75"
        />
        {{ t('secretNotes.unlock.button') }}
      </Button>
      <button
        v-if="status.has_recovery"
        type="button"
        class="mt-3 cursor-pointer text-[11px] text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
        @click="toggleRecovery"
      >
        {{ useRecovery ? t('secretNotes.unlock.usePassword') : t('secretNotes.unlock.useRecovery') }}
      </button>
    </form>
  </div>
</template>
