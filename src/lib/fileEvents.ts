// Names of the app-wide file events.
//
// They live in their own module because both ends need them and the ends sit on
// opposite sides of an import: the fileTree STORE broadcasts moves and deletes,
// while lib/fileActions (which imports that store) broadcasts the rest. Putting
// the names here keeps that from becoming an import cycle.
//
// Every one of these exists because a file can be acted on from more than one
// webview — the main window and any number of file windows — and each webview
// has its own component state to keep honest.

/** `{ projectPath, path }` — insert an `@path` mention in the run composer. */
export const FILE_MENTION_EVENT = 'file:mention'

/** `{ projectPath, path }` — hand an image to the compare host (main window). */
export const FILE_COMPARE_EVENT = 'file:compare'

/** The cut/copy clipboard mirror, so copy here can paste there. */
export const FILE_CLIPBOARD_EVENT = 'file:clipboard'

/** `{ projectPath, path }` — the entry is gone; windows showing it must close. */
export const FILE_DELETED_EVENT = 'file:deleted'

/** `{ projectPath, from, to }` — renamed or moved; windows should follow it. */
export const FILE_MOVED_EVENT = 'file:moved'
