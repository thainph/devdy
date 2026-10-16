<script setup lang="ts">
// A message's files, Slack style: downloaded images as inline thumbnails (click →
// lightbox), other downloaded files as cards opening with the OS default app, and
// files Devdy could not download (error / too large / unavailable / pending) as
// dimmed cards with the reason + "Open in Slack" (permalink).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { convertFileSrc } from '@tauri-apps/api/core'
import { openPath, openUrl } from '@tauri-apps/plugin-opener'
import {
  ExternalLink,
  File,
  FileArchive,
  FileCode,
  FileImage,
  FileSpreadsheet,
  FileText,
  FileVideo,
  FileAudio,
} from 'lucide-vue-next'
import { useToast } from '@/composables/useToast'
import { formatSize } from '@/lib/slackThreadUtils'
import type { SlackFile } from '@/stores/slackThreads'

const props = defineProps<{ files: SlackFile[] }>()
const emit = defineEmits<{ openImage: [fileId: string] }>()

const { t } = useI18n()
const { toast } = useToast()

const images = computed(() => props.files.filter((f) => f.isImage && f.filePath))
const others = computed(() => props.files.filter((f) => !(f.isImage && f.filePath)))

function fileIcon(f: SlackFile) {
  const mime = f.mime ?? ''
  const type = (f.filetype ?? f.name.split('.').pop() ?? '').toLowerCase()
  if (mime.startsWith('image/')) return FileImage
  if (mime.startsWith('video/')) return FileVideo
  if (mime.startsWith('audio/')) return FileAudio
  if (/^(zip|gzip|gz|tar|rar|7z)$/.test(type)) return FileArchive
  if (/^(csv|xls|xlsx|numbers|tsv)$/.test(type)) return FileSpreadsheet
  if (/^(js|ts|py|rs|go|java|json|yaml|yml|xml|html|css|sql|sh|php|rb|kt|swift)$/.test(type)) return FileCode
  if (mime.startsWith('text/') || /^(txt|md|pdf|doc|docx|rtf|text)$/.test(type)) return FileText
  return File
}

function unavailableReason(f: SlackFile): string {
  switch (f.downloadStatus) {
    case 'too_large':
      return t('slackThread.file.tooLarge')
    case 'unavailable':
      return t('slackThread.file.unavailable')
    case 'error':
      return f.downloadError ? `${t('slackThread.file.error')}: ${f.downloadError}` : t('slackThread.file.error')
    default:
      return t('slackThread.file.pending')
  }
}

function thumbStyle(f: SlackFile) {
  // Reserve the box from the known size so the list doesn't jump as images load.
  if (!f.width || !f.height) return undefined
  const scale = Math.min(1, 360 / f.width, 240 / f.height)
  return { width: `${Math.round(f.width * scale)}px`, height: `${Math.round(f.height * scale)}px` }
}

async function openFile(f: SlackFile) {
  if (f.filePath && f.downloadStatus === 'done') {
    try {
      await openPath(f.filePath)
    } catch (e) {
      toast.error(t('slackThread.file.openFailed', { error: String(e) }))
    }
  } else if (f.permalink) {
    openUrl(f.permalink).catch((e) => toast.error(String(e)))
  }
}
</script>

<template>
  <div v-if="files.length" class="mt-1.5 space-y-1.5">
    <div v-if="images.length" class="flex flex-wrap gap-2">
      <button
        v-for="f in images"
        :key="f.id"
        type="button"
        class="block max-h-[240px] max-w-[360px] cursor-zoom-in overflow-hidden rounded-md border border-border/60 bg-muted/40"
        :style="thumbStyle(f)"
        :title="f.title || f.name"
        @click="emit('openImage', f.id)"
      >
        <img
          :src="convertFileSrc(f.filePath!)"
          :alt="f.title || f.name"
          class="h-full max-h-[240px] w-full max-w-[360px] object-contain"
          loading="lazy"
        >
      </button>
    </div>

    <div v-if="others.length" class="flex flex-wrap gap-2">
      <button
        v-for="f in others"
        :key="f.id"
        type="button"
        class="flex w-[300px] max-w-full items-center gap-2.5 rounded-md border border-border/70 bg-card px-2.5 py-2 text-left transition-colors"
        :class="f.downloadStatus === 'done'
          ? 'cursor-pointer hover:border-border hover:bg-accent/40'
          : f.permalink ? 'cursor-pointer opacity-70 hover:opacity-100' : 'cursor-default opacity-60'"
        :title="f.downloadStatus === 'done' ? t('slackThread.file.open', { name: f.name }) : unavailableReason(f)"
        @click="openFile(f)"
      >
        <span class="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-muted">
          <component :is="fileIcon(f)" class="h-4.5 w-4.5 text-muted-foreground" :stroke-width="1.5" />
        </span>
        <span class="min-w-0 flex-1">
          <span class="block truncate text-[13px] font-medium text-foreground">{{ f.title || f.name }}</span>
          <span class="block truncate text-[11px] text-muted-foreground">
            <template v-if="f.downloadStatus === 'done'">
              {{ [f.filetype?.toUpperCase(), formatSize(f.size)].filter(Boolean).join(' · ') }}
            </template>
            <template v-else>{{ unavailableReason(f) }}</template>
          </span>
        </span>
        <span
          v-if="f.downloadStatus !== 'done' && f.permalink"
          class="flex shrink-0 items-center gap-1 text-[11px] text-primary"
        >
          <ExternalLink class="h-3 w-3" :stroke-width="1.75" />
          {{ t('slackThread.file.openInSlack') }}
        </span>
      </button>
    </div>
  </div>
</template>
