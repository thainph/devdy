import { createI18n } from 'vue-i18n'

/**
 * App-wide i18n. Messages are split one JSON file per namespace (usually one
 * per view/domain) under `locales/<lang>/<namespace>.json`, then auto-merged
 * here via Vite glob. This keeps each screen's strings isolated so multiple
 * files can be edited without JSON merge conflicts.
 *
 * Namespace = the file's base name. So `locales/en/settings.json` becomes the
 * `settings.*` message tree, referenced as `t('settings.title')`.
 */
export type Locale = 'en' | 'vi'

export const SUPPORTED_LOCALES: { value: Locale; label: string }[] = [
  { value: 'en', label: 'English' },
  { value: 'vi', label: 'Tiếng Việt' },
]

function buildMessages(
  files: Record<string, { default: Record<string, unknown> }>,
): Record<string, unknown> {
  const messages: Record<string, unknown> = {}
  for (const path in files) {
    // path looks like './locales/en/settings.json' → namespace 'settings'
    const namespace = path.split('/').pop()!.replace('.json', '')
    messages[namespace] = files[path].default
  }
  return messages
}

const en = buildMessages(
  import.meta.glob('./locales/en/*.json', { eager: true }) as Record<
    string,
    { default: Record<string, unknown> }
  >,
)
const vi = buildMessages(
  import.meta.glob('./locales/vi/*.json', { eager: true }) as Record<
    string,
    { default: Record<string, unknown> }
  >,
)

export const i18n = createI18n({
  legacy: false,
  locale: 'en',
  fallbackLocale: 'en',
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  messages: { en, vi } as any,
})

/** Switch the active language app-wide. Safe to call with any string. */
export function setLocale(locale: string) {
  const next: Locale = locale === 'vi' ? 'vi' : 'en'
  ;(i18n.global.locale as unknown as { value: Locale }).value = next
  document.documentElement.setAttribute('lang', next)
}
