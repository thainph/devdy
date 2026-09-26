// Which kind of webview am I?
//
// The answer is read ONCE, at module load — which happens during the initial
// import graph, before the router runs its first navigation and rewrites the URL.
// Reading `window.location.search` later (e.g. from a route component that mounts
// after navigation) can see the query already stripped, so anything that needs to
// know it is a session pop-out must import from here rather than re-parse the URL.
const params = new URLSearchParams(window.location.search)

/** This webview is a standalone single-run pop-out (see lib/sessionWindow). */
export const IS_SESSION_WINDOW = params.get('sessionWindow') === '1'

/** The project / run the session window was opened for (null in every other window). */
export const SESSION_WINDOW_PROJECT_ID = params.get('projectId')
export const SESSION_WINDOW_RUN_ID = params.get('runId')
