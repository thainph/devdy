<script setup lang="ts">
// Settings → Devices while Remote Control isn't set up: the four steps in order (relay,
// name, master password, turn on). The first step not done is open; a done step can be
// reopened with Edit.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, ListChecks, Loader2 } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { nextSetupStep, setupDone, SETUP_STEPS, type SetupStep } from '@/lib/remoteDevice'
import RelayForm from './RelayForm.vue'
import DeviceNameForm from './DeviceNameForm.vue'
import MasterPasswordForm from './MasterPasswordForm.vue'

const { t } = useI18n()
const { toast } = useToast()
const store = useRemoteControlStore()

const done = computed(() => setupDone(store.status))
const next = computed(() => nextSetupStep(store.status))
/** A done step reopened with Edit (else the next step to do is open). */
const editing = ref<SetupStep | null>(null)
const open = computed(() => editing.value ?? next.value)
const enabling = ref(false)

function summary(step: SetupStep): string {
  const s = store.status
  if (step === 'relay') return t('remote.setup.relaySummary', { url: s?.relay_url ?? '' })
  if (step === 'name') return s?.device_alias ?? ''
  if (step === 'password') return t('remote.setup.passwordSummary')
  return ''
}

/** Reopen a step (from the This machine card's "Edit relay"). */
function edit(step: SetupStep) {
  editing.value = done.value[step] ? step : null
}
defineExpose({ edit })

function saved() {
  editing.value = null
}

async function enable() {
  enabling.value = true
  try {
    await store.enable()
    toast.success(t('remote.device.toast.enabled'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    enabling.value = false
  }
}
</script>

<template>
  <Card>
    <template #header>
      <ListChecks class="h-4 w-4 text-primary" :stroke-width="1.75" />
      <h3 class="text-sm font-semibold flex-1">{{ t('remote.setup.title') }}</h3>
      <span class="text-[11px] tabular-nums text-muted-foreground">
        {{ SETUP_STEPS.filter(k => done[k]).length }}/{{ SETUP_STEPS.length }}
      </span>
    </template>
    <ol class="divide-y divide-border/50">
      <li
        v-for="(step, i) in SETUP_STEPS"
        :key="step"
        class="px-4 py-3"
        :class="[open === step && 'bg-primary/5', !done[step] && open !== step && 'opacity-60']"
      >
        <div class="flex items-center gap-3">
          <span
            class="flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold"
            :class="done[step] && open !== step
              ? 'bg-success/15 text-success'
              : open === step ? 'bg-primary text-primary-foreground' : 'border border-border text-muted-foreground'"
          >
            <Check v-if="done[step] && open !== step" class="h-3.5 w-3.5" :stroke-width="2.5" />
            <template v-else>{{ i + 1 }}</template>
          </span>
          <div class="min-w-0 flex-1">
            <p class="text-[13px] font-medium">{{ t(`remote.setup.steps.${step}`) }}</p>
            <p
              v-if="done[step] && open !== step && summary(step)"
              class="truncate text-[11px] text-muted-foreground"
              :class="step === 'relay' || step === 'name' ? 'font-mono' : ''"
            >{{ summary(step) }}</p>
            <p v-else class="text-[11px] text-muted-foreground">{{ t(`remote.setup.hints.${step}`) }}</p>
          </div>
          <Button
            v-if="done[step] && open !== step && step !== 'enable'"
            variant="outline"
            size="xs"
            @click="editing = step"
          >{{ t('common.edit') }}</Button>
          <Button
            v-else-if="step === 'enable' && !done.enable"
            :variant="open === 'enable' ? 'primary' : 'outline'"
            size="xs"
            :disabled="open !== 'enable' || enabling"
            @click="enable"
          >
            <Loader2 v-if="enabling" class="h-3 w-3 animate-spin" :stroke-width="2" />
            {{ t('remote.setup.enable') }}
          </Button>
        </div>
        <div v-if="open === step && step !== 'enable'" class="ml-9 mt-3">
          <RelayForm v-if="step === 'relay'" :cancelable="editing === 'relay'" @saved="saved" @cancel="editing = null" />
          <DeviceNameForm v-else-if="step === 'name'" :cancelable="editing === 'name'" @saved="saved" @cancel="editing = null" />
          <MasterPasswordForm v-else :cancelable="editing === 'password'" @saved="saved" @cancel="editing = null" />
        </div>
      </li>
    </ol>
  </Card>
</template>
