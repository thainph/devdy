import { registerMessages } from './core'

/**
 * The phone Controller's messages: only the namespaces its own screens and the
 * shared components it renders (StreamLog, DiffView, the item/time helpers)
 * read, instead of the whole desktop app's strings. A namespace missing here
 * shows up as raw keys in the Controller — add it to both globs.
 */
export { i18n, setLocale } from './core'

type LocaleModule = { default: Record<string, unknown> }

// Vite needs literal glob patterns, so the list is spelled out twice.
registerMessages({
  en: import.meta.glob<LocaleModule>(
    './locales/en/{controller,focus,item,misc,permission,projects,run,settings,streamLog}.json',
    { eager: true },
  ),
  vi: import.meta.glob<LocaleModule>(
    './locales/vi/{controller,focus,item,misc,permission,projects,run,settings,streamLog}.json',
    { eager: true },
  ),
})
