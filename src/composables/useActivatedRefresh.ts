import { onActivated } from 'vue'

/**
 * Run `refresh` every time a `<KeepAlive>`d view is re-shown — but NOT on the
 * first one.
 *
 * Why the skip: for a component inside `<KeepAlive>`, Vue fires `onActivated` on
 * the initial mount as well as on every re-activation. The views using this
 * already load their data in `onMounted`, so without the skip the first visit
 * would fetch everything twice.
 *
 * Why this exists at all: `<KeepAlive>` means `onMounted` runs exactly once for
 * the lifetime of the app. Anything the view read at mount would otherwise stay
 * frozen at those values no matter how long the user is away or what they change
 * on another screen. The cached component still paints instantly from the state
 * it already has; this just refreshes it underneath, so there is no loading
 * flash on return.
 *
 * Only put *refresh* work here. One-time setup (seeding a default selection,
 * binding a store) belongs in `onMounted` — re-running it on every visit would
 * throw away what the user did last time.
 */
export function useActivatedRefresh(refresh: () => void | Promise<void>) {
  let seenFirst = false
  onActivated(() => {
    if (!seenFirst) {
      seenFirst = true
      return
    }
    void refresh()
  })
}
