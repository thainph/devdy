<script setup lang="ts">
/**
 * Link entry (Phase 4): auto-parse the session link from the URL hash (a QR is a
 * clickable controller URL) and, on success, emit `link` WITHOUT connecting —
 * the parent then connects and waits for the Host OTP gate. A parse failure just
 * leaves the manual paste box for the user.
 */
import { onMounted, ref } from 'vue'
import { Link2, ShieldCheck } from 'lucide-vue-next'
import { Button, Textarea } from '@/components/ui'
import { parseSessionLink, SessionLinkParseError, type SessionLinkPayload } from '../protocol'

const emit = defineEmits<{ link: [payload: SessionLinkPayload] }>()

const manual = ref('')
const error = ref<string | null>(null)
const autoFromUrl = ref(false)

function tryLink(raw: string): void {
  error.value = null
  try {
    emit('link', parseSessionLink(raw))
  } catch (e) {
    error.value = e instanceof SessionLinkParseError ? e.message : 'Could not read the link.'
  }
}

function submitManual(): void {
  if (!manual.value.trim()) {
    error.value = 'Paste the controller link first.'
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
        <h1 class="text-lg font-semibold">Devdy Remote</h1>
        <p class="text-sm text-foreground/60">
          Open the remote link from Devdy (scan the QR or paste it below) to control this session
          securely.
        </p>
      </div>

      <div v-if="autoFromUrl" class="text-center text-sm text-foreground/60">
        Connecting from the scanned link…
      </div>

      <div class="space-y-3">
        <label class="block text-xs font-medium uppercase tracking-wider text-foreground/50">
          Controller link
        </label>
        <Textarea
          v-model="manual"
          :rows="4"
          placeholder="Paste the controller link (https://…/controller.html#…) here"
          class="font-mono text-xs"
          @keydown.meta.enter="submitManual"
          @keydown.ctrl.enter="submitManual"
        />
        <Button variant="primary" class="w-full" @click="submitManual">
          <Link2 class="h-4 w-4" :stroke-width="2" /> Continue
        </Button>
        <p v-if="error" class="text-sm text-destructive text-center">{{ error }}</p>
      </div>

      <p class="text-[11px] leading-relaxed text-foreground/40 text-center">
        The connection is end-to-end encrypted. You will enter a one-time code shown on the host to
        finish connecting. The relay never sees your data, and the host fingerprint is verified.
      </p>
    </div>
  </div>
</template>
