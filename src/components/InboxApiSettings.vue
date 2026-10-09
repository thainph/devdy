<script setup lang="ts">
// Settings → "Inbox API (Chrome extension)": the local HTTP endpoint the Chrome
// extension pushes captures to (Slack threads and web pages). Shows whether it is listening, the base URL and the
// bearer token (masked; copy / reveal / regenerate). Regenerating invalidates the
// old token immediately, so it asks first.
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Copy, Eye, EyeOff, Inbox, Loader2, RefreshCw } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useCapturesStore, type InboxApiInfo } from '@/stores/captures'

const { t } = useI18n()
const { confirm } = useConfirm()
const { toast } = useToast()
const store = useCapturesStore()

const info = ref<InboxApiInfo | null>(null)
const loading = ref(true)
const error = ref<string | null>(null)
const regenerating = ref(false)
const revealed = ref(false)
const copied = ref<'url' | 'token' | null>(null)

const maskedToken = computed(() => {
  const token = info.value?.token ?? ''
  if (!token) return ''
  if (revealed.value) return token
  return `${token.slice(0, 4)}${'•'.repeat(Math.max(8, token.length - 8))}${token.slice(-4)}`
})

const endpoints = computed(() => {
  const base = info.value?.baseUrl || 'http://127.0.0.1:<port>'
  return `POST ${base}/v1/slack-threads\nPOST ${base}/v1/web-pages`
})

async function load() {
  loading.value = true
  error.value = null
  try {
    info.value = await store.getInboxApiInfo()
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = false
  }
}

async function copy(what: 'url' | 'token') {
  const text = what === 'url' ? info.value?.baseUrl : info.value?.token
  if (!text) return
  try {
    await navigator.clipboard.writeText(text)
    copied.value = what
    setTimeout(() => {
      if (copied.value === what) copied.value = null
    }, 1500)
    toast.success(t('settings.inboxApi.copied'))
  } catch {
    /* clipboard unavailable */
  }
}

async function regenerate() {
  if (
    !(await confirm({
      title: t('settings.inboxApi.regenerateTitle'),
      message: t('settings.inboxApi.regenerateMessage'),
      confirmLabel: t('settings.inboxApi.regenerate'),
    }))
  ) {
    return
  }
  regenerating.value = true
  try {
    info.value = await store.regenerateInboxApiToken()
    toast.success(t('settings.inboxApi.regenerated'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    regenerating.value = false
  }
}

onMounted(load)
</script>

<template>
  <Card body-class="p-4 space-y-4">
    <template #header>
      <Inbox class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t('settings.inboxApi.title') }}</span>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('settings.inboxApi.help') }}</p>

    <div v-if="loading" class="h-24 animate-pulse rounded-md border border-border bg-card" />

    <p v-else-if="error" class="text-[11px] text-destructive">{{ error }}</p>

    <template v-else-if="info">
      <!-- Status -->
      <div class="space-y-1.5">
        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('settings.inboxApi.status') }}
        </label>
        <div class="flex items-center gap-2 text-xs">
          <span
            class="h-2 w-2 shrink-0 rounded-full"
            :class="info.running ? 'bg-emerald-500' : 'bg-red-500'"
          />
          <span>
            {{
              info.running
                ? t('settings.inboxApi.running', { port: info.port ?? '?' })
                : t('settings.inboxApi.stopped')
            }}
          </span>
          <button
            type="button"
            class="ml-auto cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            :title="t('settings.inboxApi.refresh')"
            @click="load"
          >
            <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
        </div>
      </div>

      <!-- Base URL -->
      <div class="space-y-1.5">
        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('settings.inboxApi.baseUrl') }}
        </label>
        <div class="flex items-center gap-1.5">
          <code class="min-w-0 flex-1 truncate rounded-md border border-border/60 bg-muted/30 px-2.5 py-1.5 font-mono text-[11px]">
            {{ info.baseUrl || '—' }}
          </code>
          <Button
            variant="outline"
            size="icon-sm"
            :disabled="!info.baseUrl"
            :title="t('settings.inboxApi.copy')"
            @click="copy('url')"
          >
            <component :is="copied === 'url' ? Check : Copy" class="h-3.5 w-3.5" :stroke-width="1.75" />
          </Button>
        </div>
      </div>

      <!-- Token -->
      <div class="space-y-1.5">
        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">
          {{ t('settings.inboxApi.token') }}
        </label>
        <div class="flex items-center gap-1.5">
          <code class="min-w-0 flex-1 truncate rounded-md border border-border/60 bg-muted/30 px-2.5 py-1.5 font-mono text-[11px]">
            {{ maskedToken || '—' }}
          </code>
          <Button
            variant="outline"
            size="icon-sm"
            :disabled="!info.token"
            :title="revealed ? t('settings.inboxApi.hide') : t('settings.inboxApi.reveal')"
            @click="revealed = !revealed"
          >
            <component :is="revealed ? EyeOff : Eye" class="h-3.5 w-3.5" :stroke-width="1.75" />
          </Button>
          <Button
            variant="outline"
            size="icon-sm"
            :disabled="!info.token"
            :title="t('settings.inboxApi.copy')"
            @click="copy('token')"
          >
            <component :is="copied === 'token' ? Check : Copy" class="h-3.5 w-3.5" :stroke-width="1.75" />
          </Button>
        </div>
        <div class="flex items-center gap-2">
          <Button variant="outline" size="xs" :disabled="regenerating" @click="regenerate">
            <component
              :is="regenerating ? Loader2 : RefreshCw"
              class="h-3 w-3"
              :class="{ 'animate-spin': regenerating }"
              :stroke-width="1.75"
            />
            {{ t('settings.inboxApi.regenerate') }}
          </Button>
          <span class="text-[11px] text-muted-foreground">{{ t('settings.inboxApi.regenerateHint') }}</span>
        </div>
      </div>

      <!-- How the extension uses it -->
      <div class="rounded-md border border-border/60 bg-muted/30 p-3 space-y-1.5 text-[11px] text-muted-foreground">
        <div class="font-medium uppercase tracking-wider">{{ t('settings.inboxApi.usage') }}</div>
        <code class="block whitespace-pre-wrap break-all font-mono">{{ endpoints }}
Authorization: Bearer &lt;token&gt;
Content-Type: application/zip | text/markdown</code>
      </div>
    </template>
  </Card>
</template>
