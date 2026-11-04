<script setup lang="ts">
// Settings → Devices once set up: what Remote Control and device sync use — relay, auth
// token, this machine's name (+ its device id) and the master password — one row each,
// edited in place.
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { KeyRound, Trash2 } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { useRemoteControlStore } from '@/stores/remoteControl'
import RelayForm from './RelayForm.vue'
import DeviceNameForm from './DeviceNameForm.vue'
import MasterPasswordForm from './MasterPasswordForm.vue'

type Row = 'relay' | 'name' | 'password'

const { t } = useI18n()
const { toast } = useToast()
const { confirm } = useConfirm()
const store = useRemoteControlStore()
const status = computed(() => store.status)
const editing = ref<Row | null>(null)
const clearing = ref(false)

/** Open the relay row (from the This machine card's "Edit relay"). */
function editRelay() {
  editing.value = 'relay'
}
defineExpose({ editRelay })

function saved(msg: string) {
  editing.value = null
  toast.success(msg)
}

async function clearPassword() {
  const ok = await confirm({
    title: t('remote.password.clearTitle'),
    message: t('remote.password.clearMessage'),
    confirmLabel: t('remote.password.clear'),
    variant: 'destructive',
  })
  if (!ok) return
  clearing.value = true
  try {
    await store.setMasterPassword('')
    toast.success(t('remote.password.cleared'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    clearing.value = false
  }
}
</script>

<template>
  <Card body-class="divide-y divide-border/50">
    <template #header>
      <KeyRound class="h-4 w-4 text-primary" :stroke-width="1.75" />
      <h3 class="text-sm font-semibold flex-1">{{ t('remote.connection.title') }}</h3>
    </template>

    <!-- Relay + token -->
    <div class="px-4 py-2.5">
      <div class="flex items-center gap-3">
        <span class="w-32 shrink-0 text-xs text-muted-foreground">{{ t('remote.connection.relay') }}</span>
        <span class="min-w-0 flex-1 truncate font-mono text-xs">{{ status?.relay_url }}</span>
        <Button v-if="editing !== 'relay'" variant="outline" size="xs" @click="editing = 'relay'">{{ t('common.edit') }}</Button>
      </div>
      <div v-if="editing === 'relay'" class="mt-3">
        <RelayForm cancelable @saved="saved(t('remote.relay.saved'))" @cancel="editing = null" />
      </div>
    </div>
    <div v-if="editing !== 'relay'" class="flex items-center gap-3 px-4 py-2.5">
      <span class="w-32 shrink-0 text-xs text-muted-foreground">{{ t('remote.relay.token') }}</span>
      <span class="flex-1 text-xs" :class="status?.has_auth_token ? 'text-success' : 'text-destructive'">
        {{ status?.has_auth_token ? t('remote.relay.tokenInKeychain') : t('remote.relay.tokenMissing') }}
      </span>
      <Button variant="outline" size="xs" @click="editing = 'relay'">{{ t('remote.connection.change') }}</Button>
    </div>

    <!-- Name + device id -->
    <div class="px-4 py-2.5">
      <div class="flex items-center gap-3">
        <span class="w-32 shrink-0 text-xs text-muted-foreground">{{ t('remote.name.label') }}</span>
        <span class="min-w-0 flex-1 truncate font-mono text-xs">
          {{ status?.device_alias }}
          <span v-if="status?.device_id" class="ml-2 font-sans text-[11px] text-muted-foreground/70">
            {{ t('remote.connection.deviceId', { id: status.device_id.slice(0, 8) }) }}
          </span>
        </span>
        <Button v-if="editing !== 'name'" variant="outline" size="xs" @click="editing = 'name'">{{ t('remote.connection.rename') }}</Button>
      </div>
      <div v-if="editing === 'name'" class="mt-3">
        <DeviceNameForm cancelable @saved="saved(t('remote.name.saved'))" @cancel="editing = null" />
      </div>
    </div>

    <!-- Master password -->
    <div class="px-4 py-2.5">
      <div class="flex items-center gap-3">
        <span class="w-32 shrink-0 text-xs text-muted-foreground">{{ t('remote.password.label') }}</span>
        <span class="flex-1 text-xs text-success">{{ t('remote.password.isSet') }}</span>
        <template v-if="editing !== 'password'">
          <Button variant="outline" size="xs" @click="editing = 'password'">{{ t('remote.connection.change') }}</Button>
          <Button
            variant="destructive-ghost"
            size="icon-sm"
            :disabled="clearing"
            :title="t('remote.password.clear')"
            :aria-label="t('remote.password.clear')"
            @click="clearPassword"
          >
            <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          </Button>
        </template>
      </div>
      <div v-if="editing === 'password'" class="mt-3">
        <MasterPasswordForm cancelable @saved="saved(t('remote.password.saved'))" @cancel="editing = null" />
      </div>
    </div>
  </Card>
</template>
