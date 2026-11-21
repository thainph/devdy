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
// Before the pane moves, the guards the router would run for that change run
// here (lib/paneGuards): `onBeforeRouteLeave` / `onBeforeRouteUpdate` of the
// screen in the pane (e.g. Skills asking before it drops unsaved edits) — a
// `false` cancels, a location redirects. The pane's route uses its own copies
// of the route records, so those guards stay the pane's and never run for the
// left half's navigations (nor the left half's for the pane).
//
// Views reading the router singleton (`import router from '@/router'`) would
// bypass this — screens that can show here use `useRouter()` / `useRoute()`.
import { computed, provide } from 'vue'
import {
  NavigationFailureType,
  RouterView,
  START_LOCATION,
  routeLocationKey,
  routerKey,
  routerViewLocationKey,
  useRouter,
  type RouteLocationNormalizedLoaded,
  type RouteLocationRaw,
  type RouteRecordNormalized,
  type Router,
} from 'vue-router'
import { paneNavigationGuards, paneRecordCopy, runGuardQueue, type GuardRecord } from '@/lib/paneGuards'

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

// The pane's copy of each route record it shows (stable, so RouterView keeps its component).
const ownRecords = new Map<RouteRecordNormalized, RouteRecordNormalized>()
function ownRecord(r: RouteRecordNormalized): RouteRecordNormalized {
  let own = ownRecords.get(r)
  if (!own) {
    own = paneRecordCopy(r as unknown as GuardRecord) as unknown as RouteRecordNormalized
    ownRecords.set(r, own)
  }
  return own
}
function inPane(r: RouteLocationNormalizedLoaded): RouteLocationNormalizedLoaded {
  return { ...r, matched: r.matched.map(ownRecord) }
}

const paneRoute = computed(() => inPane(resolveFinal(props.location)))

// `useRoute()` inside the pane: a reactive view of the pane's location, built
// the way vue-router builds the app-level one (a getter per route key).
const reactiveRoute = {} as RouteLocationNormalizedLoaded
for (const key of Object.keys(START_LOCATION)) {
  Object.defineProperty(reactiveRoute, key, {
    get: () => (paneRoute.value as unknown as Record<string, unknown>)[key],
    enumerable: true,
  })
}

/** A cancelled pane navigation, as the router's push / replace resolve one. */
function aborted(to: RouteLocationNormalizedLoaded, from: RouteLocationNormalizedLoaded, error?: unknown): Error {
  const failure = error instanceof Error ? error : new Error(`Navigation aborted from "${from.fullPath}" to "${to.fullPath}"`)
  return Object.assign(failure, { type: NavigationFailureType.aborted, from, to })
}

// The latest pane navigation wins: an older one whose guards resolve later is dropped.
let navSeq = 0

async function navigate(raw: RouteLocationRaw, replace: boolean, redirects = 0): Promise<Error | undefined> {
  const seq = ++navSeq
  const from = paneRoute.value
  const to = inPane(resolveFinal(raw, from))
  // Same place: nothing changes, so no guard asks (as the router does).
  if (to.fullPath === from.fullPath) {
    emit('navigate', to, replace)
    return undefined
  }
  const out = await runGuardQueue(
    paneNavigationGuards(
      to as unknown as { matched: GuardRecord[] },
      from as unknown as { matched: GuardRecord[] },
    ),
  )
  if (seq !== navSeq) return aborted(to, from)
  if (out.kind === 'abort') return aborted(to, from, out.error)
  if (out.kind === 'redirect') {
    if (redirects >= 5) return aborted(to, from)
    return navigate(out.to as RouteLocationRaw, replace, redirects + 1)
  }
  emit('navigate', to, replace)
  return undefined
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
