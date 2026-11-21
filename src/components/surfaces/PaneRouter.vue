<script setup lang="ts">
// A page (Dashboard, Skills, Settings… — lib/navigation `pageRouteFor`) in the
// right half of a split. Pages read and write their state through the router
// (`?id=` of the selected skill…), so this half gets a router of its own — in
// memory, over the same page components — provided to its subtree: the page's
// useRoute / useRouter / RouterView see it instead of the main window's, and
// the two halves never fight over one URL.
//
// A navigation to anything that isn't a page (a session, a surface…) leaves
// this half alone and goes to the main window's router, i.e. the left half.
import { computed, defineComponent, provide, reactive, watch, type ComputedRef } from 'vue'
import {
  RouterView,
  START_LOCATION,
  createMemoryHistory,
  createRouter,
  routeLocationKey,
  routerKey,
  routerViewLocationKey,
  viewDepthKey,
  type RouteLocationNormalizedLoaded,
  type RouteRecordRaw,
} from 'vue-router'
import mainRouter from '@/router'
import { NAV_ROUTES } from '@/lib/navigation'

const props = defineProps<{
  /** The page tab's full path (query included). */
  fullPath: string
}>()
const emit = defineEmits<{ navigated: [fullPath: string] }>()

// Every route of the main router, under the same names (pages check
// `route.name === 'skills'`, push `{ name: 'stats' }`, open a run by name…), but
// only the pages render here; the rest exist to be resolved and handed over.
const PAGE_PATHS = new Set(NAV_ROUTES.filter((r) => !r.surface).map((r) => r.path))
const Elsewhere = defineComponent({ name: 'PaneElsewhere', render: () => null })
const routes: RouteRecordRaw[] = mainRouter.getRoutes().map((rec): RouteRecordRaw =>
  PAGE_PATHS.has(rec.path)
    ? { path: rec.path, name: rec.name, component: rec.components?.default ?? Elsewhere }
    : { path: rec.path, name: rec.name, component: Elsewhere },
)

const router = createRouter({ history: createMemoryHistory(), routes })

router.beforeEach((to) => {
  if (PAGE_PATHS.has(to.path)) return true
  // Not a page: it opens on the left, in the main window's router.
  void mainRouter.push(to.fullPath).catch(() => {})
  return false
})
router.afterEach((to, _from, failure) => {
  if (!failure) emit('navigated', to.fullPath)
})

// What vue-router's own install provides app-wide, scoped to this subtree.
const reactiveRoute = {} as Record<string, ComputedRef<unknown>>
for (const key of Object.keys(START_LOCATION) as (keyof RouteLocationNormalizedLoaded)[]) {
  reactiveRoute[key] = computed(() => router.currentRoute.value[key])
}
provide(routerKey, router)
provide(routeLocationKey, reactive(reactiveRoute) as unknown as RouteLocationNormalizedLoaded)
provide(routerViewLocationKey, router.currentRoute)
provide(viewDepthKey, 0)

watch(
  () => props.fullPath,
  (path) => {
    if (router.currentRoute.value.fullPath !== path) void router.push(path).catch(() => {})
  },
  { immediate: true },
)
</script>

<template>
  <div class="h-full min-h-0 overflow-auto">
    <RouterView />
  </div>
</template>
