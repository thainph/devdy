<script setup lang="ts">
// Settings → Devices: the relay card (relay, device name, master password), then Remote Control.
// Backup is its own Settings section (SettingsBackup.vue).
import DeviceRelaySettings from '@/components/remote/DeviceRelaySettings.vue'
import RemoteControlSettings from '@/components/remote/RemoteControlSettings.vue'
</script>

<template>
  <div class="space-y-4">
    <DeviceRelaySettings />
    <RemoteControlSettings />
  </div>
</template>
