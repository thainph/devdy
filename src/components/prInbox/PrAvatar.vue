<script setup lang="ts">
// Round GitHub-style avatar for the PR inbox: the remote avatar image, or the
// login's initials on a stable tone when there is none / it fails to load.
// SlackAvatar's sibling (same initials + avatarTone helpers), but takes a URL
// rather than a cached file path and is round like GitHub's.
import { computed, ref, watch } from 'vue'
import { avatarTone, initials } from '@/lib/slackThreadUtils'

const props = withDefaults(
  defineProps<{
    url: string | null
    name: string
    /** Pixel size (square). */
    size?: number
  }>(),
  { size: 20 },
)

const failed = ref(false)
watch(() => props.url, () => { failed.value = false })

const src = computed(() => (props.url && !failed.value ? props.url : null))
</script>

<template>
  <span
    class="flex shrink-0 items-center justify-center overflow-hidden rounded-full text-white font-semibold select-none"
    :class="src ? 'bg-muted' : avatarTone(name.toLowerCase())"
    :style="{ width: `${size}px`, height: `${size}px`, fontSize: `${Math.max(8, Math.round(size * 0.38))}px` }"
    aria-hidden="true"
  >
    <img v-if="src" :src="src" alt="" class="h-full w-full object-cover" @error="failed = true">
    <template v-else>{{ initials(name) }}</template>
  </span>
</template>
