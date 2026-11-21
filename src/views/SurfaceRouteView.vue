<script setup lang="ts">
// Route component of every surface tab (/pr-inbox, /calendar, /todos, /file…):
// reads the tab's params from the query and hands them to SurfaceHost.
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import SurfaceHost from '@/components/surfaces/SurfaceHost.vue'
import { cleanParams, type SurfaceKind } from '@/lib/surfaces'

defineOptions({ name: 'SurfaceRouteView' })

const props = defineProps<{ surface: SurfaceKind }>()
const route = useRoute()
const params = computed(() => cleanParams(props.surface, route.query))
</script>

<template>
  <SurfaceHost :surface="surface" :params="params" class="h-full" />
</template>
