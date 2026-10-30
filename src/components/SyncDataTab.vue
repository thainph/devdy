<script setup lang="ts">
// Settings → Sync → Data: which data categories this machine receives on Sync (machine-local,
// `sync_prefs_set_categories` with the disabled ids). `secrets` nests under Accounts and also drives
// the existing secrets switch (`sync_set_secrets`).
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Info } from 'lucide-vue-next'
import { useToast } from '@/composables/useToast'
import { useSyncStore } from '@/stores/sync'
import {
  SYNC_CATEGORIES,
  SYNC_CATEGORY_IDS,
  allCategoriesOn,
  setCategory,
  syncErrorCode,
  syncGetSecrets,
  syncPrefsSetCategories,
  syncSetSecrets,
} from '@/lib/sync'

const { t } = useI18n()
const { toast } = useToast()
const store = useSyncStore()

const busy = ref(false)
/** The legacy secrets switch; `secrets` counts as off when either says so. */
const secretsSwitch = ref(true)

const off = computed(() => {
  const list = store.prefs?.categories_off ?? []
  return secretsSwitch.value || list.includes('secrets') ? list : setCategory(list, 'secrets', false)
})
const allOn = computed(() => allCategoriesOn(off.value))

function isOn(id: string): boolean {
  return !off.value.includes(id)
}

function parentOff(parent: string | undefined): boolean {
  return !!parent && !isOn(parent)
}

function errorText(e: unknown): string {
  const code = syncErrorCode(e)
  return code ? t(`settings.sync.errors.${code}`) : String(e)
}

async function save(next: string[]) {
  if (busy.value) return
  busy.value = true
  try {
    await syncPrefsSetCategories(next)
    const secrets = !next.includes('secrets')
    if (secrets !== secretsSwitch.value) {
      await syncSetSecrets(secrets)
      secretsSwitch.value = secrets
    }
    if (store.prefs) store.prefs = { ...store.prefs, categories_off: next }
  } catch (e) {
    toast.error(errorText(e))
  } finally {
    busy.value = false
  }
}

function toggle(id: string, on: boolean) {
  void save(setCategory(off.value, id, on))
}

function toggleAll(on: boolean) {
  void save(on ? [] : [...SYNC_CATEGORY_IDS])
}

onMounted(async () => {
  secretsSwitch.value = await syncGetSecrets().catch(() => true)
  if (!store.prefs) await store.loadPrefs().catch((e: unknown) => toast.error(errorText(e)))
})
</script>

<template>
  <section class="space-y-3">
    <p class="text-[11px] text-muted-foreground">{{ t('settings.sync.data.help') }}</p>
    <label class="flex items-center gap-2 text-xs font-medium">
      <input
        type="checkbox"
        class="accent-primary"
        :checked="allOn"
        :disabled="busy || !store.prefs"
        @change="toggleAll(($event.target as HTMLInputElement).checked)"
      >
      {{ t('settings.sync.data.selectAll') }}
    </label>
    <ul class="divide-y divide-border/60 rounded-lg border border-border/60">
      <li
        v-for="c in SYNC_CATEGORIES"
        :key="c.id"
        class="px-3 py-2"
        :class="{ 'pl-9': c.parent, 'opacity-50': parentOff(c.parent) }"
      >
        <label class="flex cursor-pointer items-start gap-2">
          <input
            type="checkbox"
            class="mt-0.5 accent-primary"
            :checked="isOn(c.id)"
            :disabled="busy || !store.prefs || parentOff(c.parent)"
            @change="toggle(c.id, ($event.target as HTMLInputElement).checked)"
          >
          <span class="min-w-0">
            <span class="block text-sm">{{ t(`settings.sync.data.categories.${c.id}.label`) }}</span>
            <span class="block text-[11px] text-muted-foreground">{{ t(`settings.sync.data.categories.${c.id}.hint`) }}</span>
          </span>
        </label>
      </li>
    </ul>
    <p class="flex items-start gap-1.5 rounded-lg border border-sky-500/40 bg-sky-500/5 px-3 py-2 text-[11px] text-muted-foreground">
      <Info class="mt-px h-3 w-3 shrink-0 text-sky-500" :stroke-width="1.75" />
      {{ t('settings.sync.data.note') }}
    </p>
  </section>
</template>
