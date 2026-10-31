<script setup lang="ts">
// Settings → Devices: the relay card (relay, device name, master password), then a tab
// each for Remote Control and for the offline backup.
import { useI18n } from 'vue-i18n'
import DeviceRelaySettings from '@/components/remote/DeviceRelaySettings.vue'
import RemoteControlSettings from '@/components/remote/RemoteControlSettings.vue'
import SettingsBackup from '@/components/SettingsBackup.vue'

export type DevicesTab = 'remote' | 'backup'

const tab = defineModel<DevicesTab>('tab', { default: 'remote' })
const TABS: DevicesTab[] = ['remote', 'backup']
const { t } = useI18n()
</script>

<template>
  <div class="space-y-4">
    <DeviceRelaySettings />

    <div class="flex border-b border-border/60 text-xs font-medium" role="tablist">
      <button
        v-for="id in TABS"
        :key="id"
        type="button"
        role="tab"
        :aria-selected="tab === id"
        class="px-3 py-2 transition-colors cursor-pointer"
        :class="tab === id ? 'text-foreground border-b-2 border-primary' : 'text-muted-foreground hover:text-foreground'"
        @click="tab = id"
      >
        {{ t(`settings.devicesTabs.${id}`) }}
      </button>
    </div>

    <RemoteControlSettings v-if="tab === 'remote'" />
    <SettingsBackup v-else />
  </div>
</template>
