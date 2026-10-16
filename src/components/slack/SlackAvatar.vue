<script setup lang="ts">
// A Slack-style rounded-square avatar: the cached image (asset protocol), or the
// sender's initials on a tone picked from their id when there is none / it fails.
import { computed, ref, watch } from 'vue'
import { convertFileSrc } from '@tauri-apps/api/core'
import { avatarTone, initials } from '@/lib/slackThreadUtils'

const props = withDefaults(
  defineProps<{
    /** Absolute path of the cached avatar, or null. */
    path: string | null
    name: string
    /** Stable key for the fallback tone (user / bot id). */
    toneKey: string
    size?: number
  }>(),
  { size: 36 },
)

const failed = ref(false)
watch(
  () => props.path,
  () => {
    failed.value = false
  },
)

const src = computed(() => (props.path && !failed.value ? convertFileSrc(props.path) : null))
</script>

<template>
  <span
    class="flex shrink-0 items-center justify-center overflow-hidden rounded-md text-white font-semibold select-none"
    :class="src ? 'bg-muted' : avatarTone(toneKey)"
    :style="{ width: `${size}px`, height: `${size}px`, fontSize: `${Math.round(size * 0.36)}px` }"
    aria-hidden="true"
  >
    <img v-if="src" :src="src" alt="" class="h-full w-full object-cover" @error="failed = true">
    <template v-else>{{ initials(name) }}</template>
  </span>
</template>
