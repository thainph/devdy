import { registerMessages } from './core'

/**
 * App-wide i18n. Messages are split one JSON file per namespace (usually one
 * per view/domain) under `locales/<lang>/<namespace>.json`, then auto-merged
 * here via Vite glob. This keeps each screen's strings isolated so multiple
 * files can be edited without JSON merge conflicts.
 *
 * Namespace = the file's base name. So `locales/en/settings.json` becomes the
 * `settings.*` message tree, referenced as `t('settings.title')`.
 *
 * Importing this module registers EVERY namespace; the phone Controller uses
 * `./controller` instead (see `./core`).
 */
export { i18n, setLocale, SUPPORTED_LOCALES, type Locale } from './core'

type LocaleModule = { default: Record<string, unknown> }

registerMessages({
  en: import.meta.glob<LocaleModule>('./locales/en/*.json', { eager: true }),
  vi: import.meta.glob<LocaleModule>('./locales/vi/*.json', { eager: true }),
})
