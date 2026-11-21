<script setup lang="ts">
// A routed screen (a session, Dashboard, Settings, Skills…) shown in the right
// half of a split, beside the screen the app's router shows on the left.
//
// It gives the screen a router of its own: `useRoute()` inside returns this
// pane's location (not the window's), and `useRouter()` returns a proxy of the
// app router whose push / replace navigate THIS pane — they emit `navigate`
// with the resolved location, and the parent decides what the pane shows next
// (lib/tabActions `navigateRightPane`). Everything else (resolve, back…) is the
// app router's. `<RouterLink>` goes through the same two injections, so links
// inside the pane stay in the pane too.
//
// Views reading the router singleton (`import router from '@/router'`) would
// bypass this — screens that can show here use `useRouter()` / `useRoute()`.
import { computed, provide } from 'vue'
import {
  RouterView,
  START_LOCATION,
  routeLocationKey,
  routerKey,
  routerViewLocationKey,
  useRouter,
  type RouteLocationNormalizedLoaded,
  type RouteLocationRaw,
  type Router,
} from 'vue-router'

const props = defineProps<{
  /** Full path this pane shows. */
  location: string
}>()

const emit = defineEmits<{
  navigate: [to: RouteLocationNormalizedLoaded, replace: boolean]
}>()

const appRouter = useRouter()

/** Follow route-record redirects (`/skills/new` → `/skills?new=1`), like a real navigation would. */
function resolveFinal(to: RouteLocationRaw, from?: RouteLocationNormalizedLoaded): RouteLocationNormalizedLoaded {
  let r = appRouter.resolve(to, from) as unknown as RouteLocationNormalizedLoaded
  for (let i = 0; i < 5; i++) {
    const record = r.matched[r.matched.length - 1]
    const redirect = record?.redirect
    if (!redirect) break
    const next = typeof redirect === 'function' ? redirect(r, r) : redirect
    r = appRouter.resolve(
      typeof next === 'string' && !next.startsWith('/') ? { path: next } : (next as RouteLocationRaw),
      r,
    ) as unknown as RouteLocationNormalizedLoaded
  }
  return r
}

const paneRoute = computed(() => resolveFinal(props.location))

// `useRoute()` inside the pane: a reactive view of the pane's location, built
// the way vue-router builds the app-level one (a getter per route key).
const reactiveRoute = {} as RouteLocationNormalizedLoaded
for (const key of Object.keys(START_LOCATION)) {
  Object.defineProperty(reactiveRoute, key, {
    get: () => (paneRoute.value as unknown as Record<string, unknown>)[key],
    enumerable: true,
  })
}

function navigate(to: RouteLocationRaw, replace: boolean): Promise<undefined> {
  emit('navigate', resolveFinal(to, paneRoute.value), replace)
  return Promise.resolve(undefined)
}

const paneRouter = new Proxy(appRouter, {
  get(target, key, receiver) {
    if (key === 'push') return (to: RouteLocationRaw) => navigate(to, false)
    if (key === 'replace') return (to: RouteLocationRaw) => navigate(to, true)
    if (key === 'currentRoute') return paneRoute
    // Relative locations (`{ query }`) resolve against the pane, not the window.
    if (key === 'resolve') {
      return (to: RouteLocationRaw, from?: RouteLocationNormalizedLoaded) => target.resolve(to, from ?? paneRoute.value)
    }
    return Reflect.get(target, key, receiver)
  },
}) as Router

provide(routerKey, paneRouter)
provide(routeLocationKey, reactiveRoute)
provide(routerViewLocationKey, paneRoute)
</script>

<template>
  <RouterView :route="paneRoute" />
</template>
