<script setup lang="ts">
// Run-status pill. Maps the app's run statuses to Badge tones + localized
// labels and adds the pulsing dot for the running state, so every screen
// renders statuses the same way.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import Badge from './Badge.vue'

const props = withDefaults(defineProps<{
  status: string
  size?: 'xs' | 'sm'
  // Lets us disambiguate the shared 'fetched' status: for a chat session it
  // means "created, not run yet" (a draft), not "content fetched".
  runType?: string
}>(), { size: 'sm' })

const { t } = useI18n()

const TONE: Record<string, 'running' | 'success' | 'error' | 'warning' | 'info' | 'neutral'> = {
  running: 'running',
  done: 'success',
  failed: 'error',
  cancelled: 'warning',
  fetched: 'info',
}

// A freshly-created session is a draft — render it as a muted chip rather than
// the "info" tone used for a fetched issue/PR.
const isSessionDraft = computed(() => props.status === 'fetched' && props.runType === 'session')

const tone = computed(() => (isSessionDraft.value ? 'neutral' : (TONE[props.status] ?? 'neutral')))

const label = computed(() => {
  const key = isSessionDraft.value ? 'common.status.draft' : `common.status.${props.status}`
  const translated = t(key)
  // Fall back to the raw status if there's no translation for it.
  return translated === key ? props.status : translated
})
</script>

<template>
  <Badge :tone="tone" :size="size">
    <span v-if="status === 'running'" class="h-1.5 w-1.5 rounded-full bg-current animate-pulse" />
    {{ label }}
  </Badge>
</template>
