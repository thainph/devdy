<script setup lang="ts">
// Set / change this machine's master password: typed twice, checked inline. Used by the
// setup checklist, the Connection card and the pairing modal (pairing needs one).
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertCircle, Loader2 } from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import { useRemoteControlStore } from '@/stores/remoteControl'

const props = withDefaults(defineProps<{
  /** Primary button label (defaults to "Save password"). */
  submitLabel?: string
  /** Show a Cancel button next to it. */
  cancelable?: boolean
}>(), { submitLabel: undefined, cancelable: false })

const emit = defineEmits<{ saved: []; cancel: [] }>()

const MIN_LENGTH = 6

const { t } = useI18n()
const store = useRemoteControlStore()
const password = ref('')
const confirm = ref('')
const saving = ref(false)
const saveError = ref('')

const error = computed(() => {
  if (password.value && password.value.length < MIN_LENGTH) return t('remote.password.tooShort', { n: MIN_LENGTH })
  if (confirm.value && confirm.value !== password.value) return t('remote.password.mismatch')
  return ''
})
const canSave = computed(() => !!password.value && password.value === confirm.value && !error.value && !saving.value)

async function save() {
  if (!canSave.value) return
  saving.value = true
  saveError.value = ''
  try {
    await store.setMasterPassword(password.value)
    password.value = ''
    confirm.value = ''
    emit('saved')
  } catch (e) {
    saveError.value = String(e)
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <form class="max-w-sm space-y-2.5" @submit.prevent="save">
    <div class="space-y-1.5">
      <label class="text-xs font-medium text-muted-foreground">{{ t('remote.password.label') }}</label>
      <Input v-model="password" type="password" autocomplete="new-password" :disabled="saving" />
    </div>
    <div class="space-y-1.5">
      <label class="text-xs font-medium text-muted-foreground">{{ t('remote.password.confirm') }}</label>
      <Input
        v-model="confirm"
        type="password"
        autocomplete="new-password"
        :disabled="saving"
        :aria-invalid="!!error || undefined"
        :class="error ? 'border-destructive' : ''"
      />
      <p v-if="error || saveError" class="flex items-center gap-1 text-[11px] text-destructive">
        <AlertCircle class="h-3 w-3 shrink-0" :stroke-width="2" />{{ error || saveError }}
      </p>
    </div>
    <p class="text-[11px] text-muted-foreground/80">{{ t('remote.password.hint') }}</p>
    <div class="flex items-center gap-2">
      <Button type="submit" size="sm" :disabled="!canSave">
        <Loader2 v-if="saving" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
        {{ props.submitLabel ?? t('remote.password.save') }}
      </Button>
      <Button v-if="cancelable" variant="ghost" size="sm" :disabled="saving" @click="emit('cancel')">
        {{ t('common.cancel') }}
      </Button>
    </div>
  </form>
</template>
