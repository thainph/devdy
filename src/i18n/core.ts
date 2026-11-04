import { createI18n } from 'vue-i18n'

/**
 * The i18n instance WITHOUT any messages. Each entry point registers the
 * namespaces it needs: the desktop app every one of them (`@/i18n`), the phone
 * Controller only its own handful (`@/i18n/controller`) so it does not download
 * the whole app's strings.
 *
 * Modules that end up in the Controller bundle must import from here, not from
 * `@/i18n` — that one pulls every locale file in with it.
 */
export type Locale = 'en' | 'vi'

export const SUPPORTED_LOCALES: { value: Locale; label: string }[] = [
  { value: 'en', label: 'English' },
  { value: 'vi', label: 'Tiếng Việt' },
]

export const i18n = createI18n({
  legacy: false,
  locale: 'en',
  fallbackLocale: 'en',
  messages: { en: {}, vi: {} },
})

type LocaleFiles = Record<string, { default: Record<string, unknown> }>

function buildMessages(files: LocaleFiles): Record<string, unknown> {
  const messages: Record<string, unknown> = {}
  for (const path in files) {
    // path looks like './locales/en/settings.json' → namespace 'settings'
    const namespace = path.split('/').pop()!.replace('.json', '')
    messages[namespace] = files[path].default
  }
  return messages
}

/** Register the namespaces in `files` (Vite glob results) for each locale. */
export function registerMessages(files: Record<Locale, LocaleFiles>): void {
  for (const locale of Object.keys(files) as Locale[]) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    i18n.global.mergeLocaleMessage(locale, buildMessages(files[locale]) as any)
  }
}

/** Switch the active language app-wide. Safe to call with any string. */
export function setLocale(locale: string) {
  const next: Locale = locale === 'vi' ? 'vi' : 'en'
  ;(i18n.global.locale as unknown as { value: Locale }).value = next
  document.documentElement.setAttribute('lang', next)
}
