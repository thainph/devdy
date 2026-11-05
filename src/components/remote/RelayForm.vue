<script setup lang="ts">
// Relay URL + host auth token, checked inline. The token field left blank keeps the
// saved one (it lives in the Keychain and is never shown back).
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertCircle, Loader2 } from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import { useRemoteControlStore } from '@/stores/remoteControl'

defineProps<{ cancelable?: boolean }>()
const emit = defineEmits<{ saved: []; cancel: [] }>()

const { t } = useI18n()
const store = useRemoteControlStore()
const url = ref(store.status?.relay_url ?? '')
const token = ref('')
const saving = ref(false)
const saveError = ref('')
const touched = ref(false)

const hasToken = computed(() => !!store.status?.has_auth_token)
const urlError = computed(() => {
  const v = url.value.trim()
  if (!v) return touched.value ? t('remote.relay.urlRequired') : ''
  return /^wss?:\/\/\S+$/i.test(v) ? '' : t('remote.relay.urlInvalid')
})
const tokenError = computed(() => (touched.value && !hasToken.value && !token.value.trim() ? t('remote.relay.tokenRequired') : ''))

async function save() {
  touched.value = true
  if (!url.value.trim() || urlError.value || tokenError.value || saving.value) return
  saving.value = true
  saveError.value = ''
  try {
    await store.setConfig(url.value.trim(), token.value)
    token.value = ''
    emit('saved')
  } catch (e) {
    saveError.value = String(e)
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <form class="max-w-md space-y-2.5" @submit.prevent="save">
    <div class="space-y-1.5">
      <label class="text-xs font-medium text-muted-foreground">{{ t('remote.relay.url') }}</label>
      <Input
        v-model="url"
        :placeholder="t('remote.relay.urlPlaceholder')"
        spellcheck="false"
        autocomplete="off"
        class="font-mono"
        :disabled="saving"
        :class="urlError ? 'border-destructive' : ''"
        :aria-invalid="!!urlError || undefined"
      />
      <p v-if="urlError" class="flex items-center gap-1 text-[11px] text-destructive">
        <AlertCircle class="h-3 w-3 shrink-0" :stroke-width="2" />{{ urlError }}
      </p>
    </div>
    <div class="space-y-1.5">
      <label class="text-xs font-medium text-muted-foreground">
        {{ t('remote.relay.token') }}
        <span v-if="hasToken" class="font-normal text-success">· {{ t('remote.relay.tokenSaved') }}</span>
      </label>
      <Input
        v-model="token"
        type="password"
        autocomplete="off"
        :placeholder="hasToken ? t('remote.relay.tokenKeep') : ''"
        :disabled="saving"
        :class="tokenError ? 'border-destructive' : ''"
        :aria-invalid="!!tokenError || undefined"
      />
      <p v-if="tokenError || saveError" class="flex items-center gap-1 text-[11px] text-destructive">
        <AlertCircle class="h-3 w-3 shrink-0" :stroke-width="2" />{{ tokenError || saveError }}
      </p>
      <p v-else class="text-[11px] text-muted-foreground/80">{{ t('remote.relay.tokenHint') }}</p>
    </div>
    <div class="flex items-center gap-2">
      <Button type="submit" size="sm" :disabled="saving">
        <Loader2 v-if="saving" class="h-3.5 w-3.5 animate-spin" :stroke-width="1.75" />
        {{ t('common.save') }}
      </Button>
      <Button v-if="cancelable" variant="ghost" size="sm" :disabled="saving" @click="emit('cancel')">
        {{ t('common.cancel') }}
      </Button>
    </div>
  </form>
</template>
