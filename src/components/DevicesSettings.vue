<script setup lang="ts">
// Settings → Devices: "This machine" (status + the one action that fits), then the setup
// checklist until the relay, name and master password are set (the Connection card
// after that), the phone (pairing, Web Push), device sync and the folded activity log.
// Backup is its own Settings section (SettingsBackup.vue).
import { computed, nextTick, onMounted, onUnmounted, ref } from 'vue'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { setupDone } from '@/lib/remoteDevice'
import ThisDeviceCard from '@/components/remote/ThisDeviceCard.vue'
import RemoteSetupChecklist from '@/components/remote/RemoteSetupChecklist.vue'
import RemoteConnectionCard from '@/components/remote/RemoteConnectionCard.vue'
import RemotePhoneCard from '@/components/remote/RemotePhoneCard.vue'
import RemoteActivityCard from '@/components/remote/RemoteActivityCard.vue'
import RemoteSessionModal from '@/components/remote/RemoteSessionModal.vue'
import DeviceSyncCard from '@/components/deviceSync/DeviceSyncCard.vue'
import DeviceSyncDialog from '@/components/deviceSync/DeviceSyncDialog.vue'

const store = useRemoteControlStore()

const done = computed(() => setupDone(store.status))
/** The checklist stays until relay, name and password are set; switching on is the card's job. */
const inSetup = computed(() => !store.loaded || !done.value.relay || !done.value.name || !done.value.password)

const checklistEl = ref<HTMLElement | null>(null)
const checklist = ref<InstanceType<typeof RemoteSetupChecklist> | null>(null)
const connection = ref<InstanceType<typeof RemoteConnectionCard> | null>(null)
const pairOpen = ref(false)
const syncOpen = ref(false)

function scrollToSetup() {
  checklistEl.value?.scrollIntoView({ behavior: 'smooth', block: 'start' })
}

async function editRelay() {
  if (connection.value) connection.value.editRelay()
  else checklist.value?.edit('relay')
  await nextTick()
  scrollToSetup()
}

let stopWatch: (() => void) | null = null
let pollTimer: ReturnType<typeof setInterval> | null = null
onMounted(() => {
  // Live status (lifecycle + device-conflict events) shared with the sidebar dot, plus a
  // faster poll while this screen is open: relay connectivity has no event of its own.
  stopWatch = store.watchStatus()
  store.refreshAudit().catch(() => {})
  pollTimer = setInterval(() => store.refreshStatus().catch(() => {}), 5000)
})
onUnmounted(() => {
  stopWatch?.()
  if (pollTimer) clearInterval(pollTimer)
})
</script>

<template>
  <div class="space-y-4">
    <ThisDeviceCard @setup="scrollToSetup" @pair="pairOpen = true" @sync="syncOpen = true" @edit-relay="editRelay" />
    <div ref="checklistEl" class="scroll-mt-4">
      <RemoteSetupChecklist v-if="inSetup" ref="checklist" />
      <RemoteConnectionCard v-else ref="connection" />
    </div>
    <RemotePhoneCard v-if="!inSetup" @pair="pairOpen = true" />
    <DeviceSyncCard @sync="syncOpen = true" />
    <RemoteActivityCard v-if="!inSetup" />

    <RemoteSessionModal v-if="pairOpen" :open="pairOpen" :run-id="null" @close="pairOpen = false" @open-settings="pairOpen = false" />
    <Teleport to="body">
      <DeviceSyncDialog v-if="syncOpen" :open="syncOpen" @close="syncOpen = false" />
    </Teleport>
  </div>
</template>
