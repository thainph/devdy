<script setup lang="ts">
// "Watch in Pulse" toggle for a Slack thread or GitHub issue (§3.2). `label`
// is the bordered xs header control of the Slack threads window; `icon` the
// icon-only outline button of RunView's header. When the source has no project
// yet, a small dialog asks for one first and reports it (`projectChosen`) so
// the host can link the thread to it as well.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Activity, Eye } from 'lucide-vue-next'
import { Button, Modal } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import { usePulseWatch, type PulseSource } from '@/composables/usePulseWatch'

const props = withDefaults(
  defineProps<{
    source: PulseSource
    /** Project of the context (thread / Gantt / run), null when it has none. */
    projectId: string | null
    variant?: 'label' | 'icon'
    /** Tooltip / aria label when not watched yet. */
    watchLabel?: string
    /** Tooltip / aria label while watched. */
    watchingLabel?: string
  }>(),
  { variant: 'label', watchLabel: undefined, watchingLabel: undefined },
)

const emit = defineEmits<{ projectChosen: [projectId: string] }>()

const { t } = useI18n()
const { watchOf, watch, toggle } = usePulseWatch()

const current = computed(() => watchOf(props.source))
const busy = ref(false)
const picking = ref(false)
const picked = ref<string | null>(null)

const hint = computed(() =>
  current.value
    ? (props.watchingLabel ?? t('pulse.watch.watchingHint'))
    : (props.watchLabel ?? t('pulse.watch.watchHint')),
)

async function onClick() {
  if (busy.value || !props.source.url) return
  if (!current.value && !props.projectId) {
    picked.value = null
    picking.value = true
    return
  }
  busy.value = true
  try {
    await toggle(props.source, props.projectId)
  } finally {
    busy.value = false
  }
}

async function confirmProject() {
  const pid = picked.value
  if (!pid) return
  picking.value = false
  emit('projectChosen', pid)
  busy.value = true
  try {
    await watch(props.source, pid)
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <Button
    v-if="variant === 'icon'"
    :variant="current ? 'primary' : 'outline'"
    size="icon"
    class="shrink-0"
    :disabled="busy || !source.url"
    :aria-pressed="!!current"
    :title="hint"
    :aria-label="hint"
    @click="onClick"
  >
    <Activity class="h-4 w-4" :stroke-width="1.75" />
  </Button>
  <Button
    v-else
    :variant="current ? 'primary' : 'outline'"
    size="xs"
    class="shrink-0"
    :disabled="busy || !source.url"
    :aria-pressed="!!current"
    :title="hint"
    @click="onClick"
  >
    <Eye class="h-3 w-3" :stroke-width="1.75" />
    {{ current ? t('pulse.watch.watching') : t('pulse.watch.watch') }}
  </Button>

  <Modal :open="picking" :title="t('pulse.watch.chooseProjectTitle')" size="sm" @close="picking = false">
    <div class="space-y-3 p-4">
      <p class="text-xs text-muted-foreground">{{ t('pulse.watch.chooseProjectHint') }}</p>
      <ProjectSelect v-model="picked" variant="default" size="sm" />
    </div>
    <template #footer>
      <Button variant="ghost" @click="picking = false">{{ t('common.cancel') }}</Button>
      <Button :disabled="!picked" @click="confirmProject">{{ t('pulse.watch.watch') }}</Button>
    </template>
  </Modal>
</template>
