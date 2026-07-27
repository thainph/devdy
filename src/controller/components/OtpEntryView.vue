<script setup lang="ts">
/**
 * OTP entry (Phase 4): the user types the one-time code shown on the Host screen
 * to finish the E2E handshake. The OTP is a KDF input (not a compared value), so
 * a wrong code just yields a different key and the connection never opens — we
 * surface that as a retryable error. The Host limits attempts and closes the
 * room after 5 bad codes.
 */
import { computed, nextTick, ref, watch } from 'vue'
import { KeyRound, ShieldCheck, Loader2 } from 'lucide-vue-next'
import { Button } from '@/components/ui'

const props = withDefaults(
  defineProps<{
    /** Short host fingerprint for user assurance (e.g. first 12 hex chars). */
    shortFingerprint?: string
    /** True while a submitted OTP is being verified with the host. */
    connecting?: boolean
    /** Inline error (wrong/expired code) to re-prompt on. */
    error?: string | null
  }>(),
  { shortFingerprint: '', connecting: false, error: null },
)

const emit = defineEmits<{ otp: [code: string] }>()

const code = ref('')
const inputEl = ref<HTMLInputElement | null>(null)

const canSubmit = computed(() => code.value.trim().length >= 4 && !props.connecting)

function onInput(e: Event): void {
  // Accept any secret: the numeric one-time code OR the owner's master password
  // (which may contain letters/symbols). Both are fed to the KDF as-is.
  const raw = (e.target as HTMLInputElement).value
  code.value = raw.slice(0, 128)
}

function submit(): void {
  if (!canSubmit.value) return
  emit('otp', code.value.trim())
}

// Clear the field after a failed attempt so the user can retype cleanly.
watch(
  () => props.error,
  (err) => {
    if (err) {
      code.value = ''
      nextTick(() => inputEl.value?.focus())
    }
  },
)
</script>

<template>
  <div class="min-h-dvh flex items-center justify-center px-4 py-10 bg-background text-foreground">
    <div class="w-full max-w-sm space-y-6">
      <div class="text-center space-y-2">
        <div class="inline-flex h-12 w-12 items-center justify-center rounded-xl bg-primary/15 text-primary">
          <KeyRound class="h-6 w-6" :stroke-width="1.75" />
        </div>
        <h1 class="text-lg font-semibold">Enter password or code</h1>
        <p class="text-sm text-foreground/60">
          Type your remote master password, or the one-time code shown on your
          Devdy host, to finish connecting.
        </p>
      </div>

      <div
        v-if="shortFingerprint"
        class="flex items-center justify-center gap-1.5 text-[11px] font-mono text-foreground/45"
      >
        <ShieldCheck class="h-3.5 w-3.5 text-emerald-500" :stroke-width="2" />
        host {{ shortFingerprint }}…
      </div>

      <div class="space-y-3">
        <input
          ref="inputEl"
          :value="code"
          type="password"
          autocomplete="one-time-code"
          placeholder="Password or code"
          :disabled="connecting"
          class="w-full rounded-lg border border-border bg-background px-4 py-3 text-center text-xl font-mono tracking-[0.15em] focus:border-ring focus:outline-none focus:ring-1 focus:ring-ring disabled:opacity-60"
          @input="onInput"
          @keydown.enter="submit"
        />
        <Button variant="primary" class="w-full" :disabled="!canSubmit" @click="submit">
          <Loader2 v-if="connecting" class="h-4 w-4 animate-spin" :stroke-width="2" />
          {{ connecting ? 'Connecting…' : 'Connect' }}
        </Button>
        <p v-if="error" class="text-sm text-destructive text-center">{{ error }}</p>
      </div>
    </div>
  </div>
</template>
