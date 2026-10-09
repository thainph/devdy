<script setup lang="ts">
// "Copy markdown" — the reading-mode leading action of the detail windows
// (note: ItemWindow, Slack thread / web page: CaptureWindow). Copies the body
// as stored, without the title; the icon turns into a tick for a moment and a
// toast confirms.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Copy } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { useToast } from '@/composables/useToast'

const props = defineProps<{ text: string }>()

const { t } = useI18n()
const { toast } = useToast()
const copied = ref(false)

async function copy() {
  try {
    await navigator.clipboard.writeText(props.text)
    copied.value = true
    setTimeout(() => {
      copied.value = false
    }, 1500)
    toast.success(t('capture.markdownCopied'))
  } catch {
    /* clipboard unavailable */
  }
}
</script>

<template>
  <Button variant="outline" size="sm" @click="copy">
    <component :is="copied ? Check : Copy" class="h-3.5 w-3.5" :stroke-width="1.75" />
    {{ t('capture.copyMarkdown') }}
  </Button>
</template>
