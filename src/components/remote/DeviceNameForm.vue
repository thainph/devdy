<script setup lang="ts">
// This machine's name (picks its password-login code and labels it in other devices'
// sync list). `initial` pre-fills a suggestion, e.g. a free name after a clash.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertCircle, Loader2 } from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { DEVICE_ALIAS_RE } from '@/lib/remoteLogin'

const props = defineProps<{ initial?: string; cancelable?: boolean; submitLabel?: string }>()
const emit = defineEmits<{ saved: []; cancel: [] }>()

const { t } = useI18n()
const store = useRemoteControlStore()
const name = ref(props.initial ?? store.status?.device_alias ?? '')
const saving = ref(false)
const saveError = ref('')

const value = computed(() => name.value.trim().toLowerCase())
const error = computed(() => {
  if (!name.value) return ''
  return value.value && DEVICE_ALIAS_RE.test(value.value) ? '' : t('remote.name.invalid')
})

async function save() {
  if (!value.value || error.value || saving.value) return
  saving.value = true
  saveError.value = ''
  try {
    await store.setDeviceAlias(value.value)
    emit('saved')
  } catch (e) {
    saveError.value = String(e)
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <form class="max-w-sm space-y-1.5" @submit.prevent="save">
    <label class="text-xs font-medium text-muted-foreground">{{ t('remote.name.label') }}</label>
    <div class="flex items-center gap-2">
      <Input
        v-model="name"
        :placeholder="t('remote.name.placeholder')"
        autocomplete="off"
        spellcheck="false"
        class="font-mono"
        :disabled="saving"
        :class="error ? 'border-destructive' : ''"
        :aria-invalid="!!error || undefined"
      />
      <Button type="submit" size="sm" :disabled="saving || !value || !!error">
        <Loader2 v-if="saving" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
        {{ submitLabel ?? t('common.save') }}
      </Button>
      <Button v-if="cancelable" variant="ghost" size="sm" :disabled="saving" @click="emit('cancel')">
        {{ t('common.cancel') }}
      </Button>
    </div>
    <p v-if="error || saveError" class="flex items-center gap-1 text-[11px] text-destructive">
      <AlertCircle class="h-3 w-3 shrink-0" :stroke-width="2" />{{ error || saveError }}
    </p>
    <p v-else class="text-[11px] text-muted-foreground/80">{{ t('remote.name.hint') }}</p>
  </form>
</template>
