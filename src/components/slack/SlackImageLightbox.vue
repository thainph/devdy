<script setup lang="ts">
// Full-window viewer for the images of a thread: Esc closes, the ‹ / › buttons
// step through every downloaded image of the thread, and "Open with the default app" hands the
// file to the OS (openPath).
import { computed, onBeforeUnmount, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { convertFileSrc } from '@tauri-apps/api/core'
import { openPath } from '@tauri-apps/plugin-opener'
import { ChevronLeft, ChevronRight, ExternalLink, X } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { formatSize } from '@/lib/slackThreadUtils'
import type { SlackFile } from '@/stores/slackThreads'

const props = defineProps<{
  /** Every downloaded image of the thread, in message order. */
  images: SlackFile[]
  /** The one on screen. */
  currentId: string
}>()

const emit = defineEmits<{ close: []; select: [fileId: string] }>()

const { t } = useI18n()
const { toast } = useToast()

const index = computed(() => Math.max(0, props.images.findIndex((f) => f.id === props.currentId)))
const current = computed(() => props.images[index.value] ?? null)

function step(delta: number) {
  const n = props.images.length
  if (n < 2) return
  emit('select', props.images[(index.value + delta + n) % n].id)
}

async function openDefault() {
  if (!current.value?.filePath) return
  try {
    await openPath(current.value.filePath)
  } catch (e) {
    toast.error(t('slackThread.file.openFailed', { error: String(e) }))
  }
}

function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    e.preventDefault()
    e.stopPropagation()
    emit('close')
  }
}

// Capture phase: the window's own shortcuts must not see these keys meanwhile.
onMounted(() => window.addEventListener('keydown', onKey, true))
onBeforeUnmount(() => window.removeEventListener('keydown', onKey, true))
</script>

<template>
  <div
    v-if="current"
    class="fixed inset-0 z-50 flex flex-col bg-black/85"
    role="dialog"
    aria-modal="true"
    @click.self="emit('close')"
  >
    <div class="flex shrink-0 items-center gap-2 px-4 py-2 text-white/90">
      <span class="min-w-0 flex-1 truncate text-sm" :title="current.name">
        {{ current.title || current.name }}
        <span class="ml-2 text-xs text-white/60">
          {{ formatSize(current.size) }}<template v-if="images.length > 1"> · {{ index + 1 }}/{{ images.length }}</template>
        </span>
      </span>
      <Button variant="outline" size="xs" @click="openDefault">
        <ExternalLink class="h-3 w-3" :stroke-width="1.75" />
        {{ t('slackThread.lightbox.openDefault') }}
      </Button>
      <button
        type="button"
        class="flex h-7 w-7 cursor-pointer items-center justify-center rounded-md text-white/80 hover:bg-white/10 hover:text-white"
        :title="t('slackThread.lightbox.close')"
        :aria-label="t('slackThread.lightbox.close')"
        @click="emit('close')"
      >
        <X class="h-4 w-4" :stroke-width="2" />
      </button>
    </div>

    <div class="relative flex min-h-0 flex-1 items-center justify-center px-14 pb-6" @click.self="emit('close')">
      <img
        :src="convertFileSrc(current.filePath!)"
        :alt="current.title || current.name"
        class="max-h-full max-w-full object-contain"
      >
      <template v-if="images.length > 1">
        <button
          type="button"
          class="absolute left-3 top-1/2 flex h-10 w-10 -translate-y-1/2 cursor-pointer items-center justify-center rounded-full bg-white/10 text-white hover:bg-white/20"
          :title="t('slackThread.lightbox.prev')"
          :aria-label="t('slackThread.lightbox.prev')"
          @click="step(-1)"
        >
          <ChevronLeft class="h-5 w-5" :stroke-width="2" />
        </button>
        <button
          type="button"
          class="absolute right-3 top-1/2 flex h-10 w-10 -translate-y-1/2 cursor-pointer items-center justify-center rounded-full bg-white/10 text-white hover:bg-white/20"
          :title="t('slackThread.lightbox.next')"
          :aria-label="t('slackThread.lightbox.next')"
          @click="step(1)"
        >
          <ChevronRight class="h-5 w-5" :stroke-width="2" />
        </button>
      </template>
    </div>
  </div>
</template>
