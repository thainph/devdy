<script setup lang="ts">
// Renders a QR code with a TTL countdown from an arbitrary string (e.g. the
// controller session link). When the countdown reaches zero it emits `expired`
// so the parent can hide/refresh the offer.
//
// The QR encodes an out-of-band secret (link_secret). By design that secret is
// meant to be scanned physically off-screen — so it is drawn to a canvas and
// NEVER written to the console/logs.
import { onMounted, onBeforeUnmount, ref, watch, computed } from 'vue'
import QRCode from 'qrcode'
import { Copy, Check } from 'lucide-vue-next'
import { Button } from '@/components/ui'

const props = withDefaults(defineProps<{
  /** Arbitrary string to encode + copy. */
  text: string
  /** Seconds the offer stays valid. */
  ttlSeconds?: number
  /** Copy-button label. */
  copyLabel?: string
  /** Caption shown above the countdown. */
  caption?: string
}>(), {
  ttlSeconds: 60,
  copyLabel: 'Copy link',
  caption: 'Scan from the Controller device.',
})

const emit = defineEmits<{ expired: [] }>()

const canvas = ref<HTMLCanvasElement | null>(null)
const remaining = ref(props.ttlSeconds)
const renderError = ref<string | null>(null)
const copied = ref(false)
let timer: ReturnType<typeof setInterval> | null = null
let copyTimer: ReturnType<typeof setTimeout> | null = null

// The exact string encoded in the QR (and copied).
const encoded = computed(() => props.text)

// Copy the same string as the QR so it can be pasted into the Controller when
// scanning isn't possible. Same secret as the QR — kept off the console/logs.
async function copyPayload() {
  try {
    await navigator.clipboard.writeText(encoded.value)
    copied.value = true
    if (copyTimer) clearTimeout(copyTimer)
    copyTimer = setTimeout(() => (copied.value = false), 2000)
  } catch {
    renderError.value = 'Could not copy to clipboard'
  }
}

async function draw() {
  if (!canvas.value) return
  renderError.value = null
  try {
    await QRCode.toCanvas(canvas.value, encoded.value, {
      width: 220,
      margin: 1,
      errorCorrectionLevel: 'M',
      color: { dark: '#0f172a', light: '#ffffff' },
    })
  } catch (e) {
    renderError.value = 'Could not render QR code'
    // Intentionally do NOT log `e` — it may echo the secret payload.
    void e
  }
}

function startCountdown() {
  if (timer) clearInterval(timer)
  remaining.value = props.ttlSeconds
  timer = setInterval(() => {
    remaining.value -= 1
    if (remaining.value <= 0) {
      remaining.value = 0
      if (timer) clearInterval(timer)
      timer = null
      emit('expired')
    }
  }, 1000)
}

onMounted(() => {
  draw()
  startCountdown()
})

// Re-render + restart the countdown when a fresh value arrives.
watch(() => encoded.value, () => {
  draw()
  startCountdown()
})

onBeforeUnmount(() => {
  if (timer) clearInterval(timer)
  if (copyTimer) clearTimeout(copyTimer)
})
</script>

<template>
  <div class="flex flex-col items-center gap-3">
    <div class="rounded-lg bg-white p-3 shadow-sm">
      <canvas ref="canvas" class="block" />
    </div>
    <p v-if="renderError" class="text-xs text-destructive">{{ renderError }}</p>
    <div class="text-center">
      <p class="text-xs text-muted-foreground">{{ caption }}</p>
      <p
        class="mt-1 text-sm font-medium"
        :class="remaining <= 10 ? 'text-destructive' : 'text-foreground'"
      >
        Expires in {{ remaining }}s
      </p>
    </div>
    <Button variant="outline" size="sm" @click="copyPayload">
      <Check v-if="copied" class="h-3.5 w-3.5 text-emerald-500" :stroke-width="2" />
      <Copy v-else class="h-3.5 w-3.5" :stroke-width="1.75" />
      {{ copied ? 'Copied!' : copyLabel }}
    </Button>
  </div>
</template>
