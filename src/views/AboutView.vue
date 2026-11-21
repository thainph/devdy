<script setup lang="ts">
import { nextTick, onMounted, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { Globe, Keyboard } from 'lucide-vue-next'
import { Badge, Card, PageHeader } from '@/components/ui'
import { getVersion } from '@tauri-apps/api/app'
import { IS_MAC } from '@/lib/appMenu'
import { keyLabel, shortcutGroupsFor } from '@/lib/shortcuts'

const { t } = useI18n()

// The app's keyboard shortcuts (lib/shortcuts), in this platform's notation.
const groups = shortcutGroupsFor(IS_MAC)
const key = (k: string) => keyLabel(k, IS_MAC)

// Help → Keyboard Shortcuts (⌘/) lands here with #shortcuts: bring the card into view.
const route = useRoute()
const shortcutsEl = ref<HTMLElement | null>(null)
watch(
  () => route.hash,
  async (hash) => {
    if (hash !== '#shortcuts') return
    await nextTick()
    shortcutsEl.value?.scrollIntoView({ behavior: 'smooth', block: 'start' })
  },
  { immediate: true },
)

// Read from Tauri so it always matches the packaged build (tauri.conf.json).
const appVersion = ref('')
onMounted(async () => {
  try {
    appVersion.value = await getVersion()
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }
})
</script>

<template>
  <div class="flex flex-col h-full">
    <PageHeader :title="t('about.title')" />

    <div class="flex-1 overflow-auto p-6">
      <div class="mx-auto flex flex-col items-center text-center max-w-xs pt-4">
        <!-- Logo -->
        <img src="/logo.png" alt="Devdy" class="h-14 w-14 rounded-2xl mb-5" />

        <h2 class="text-xl font-bold tracking-tight">Devdy</h2>
        <p class="text-sm text-muted-foreground mt-1">{{ t('about.tagline') }}</p>
        <Badge v-if="appVersion" class="mt-2 px-2 py-0.5 text-[10px] font-mono font-medium">
          v{{ appVersion }}
        </Badge>

        <p class="mt-6 text-xs text-muted-foreground leading-relaxed">
          {{ t('about.description') }}
        </p>

        <div class="mt-8 flex items-center gap-3 text-xs text-muted-foreground">
          <span class="flex items-center gap-1">
            <Globe class="h-3 w-3" :stroke-width="1.5" />
            Tauri 2
          </span>
          <span class="h-1 w-1 rounded-full bg-border" />
          <span>Vue 3</span>
          <span class="h-1 w-1 rounded-full bg-border" />
          <span>Rust</span>
        </div>
      </div>

      <!-- Keyboard shortcuts -->
      <div ref="shortcutsEl" class="scroll-mt-4" />
      <Card class="mx-auto mt-10 max-w-3xl" body-class="p-4">
        <template #header>
          <Keyboard class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
          <span class="text-xs font-semibold">{{ t('about.shortcuts.title') }}</span>
        </template>
        <div class="grid gap-x-8 gap-y-5 sm:grid-cols-2">
          <section v-for="group in groups" :key="group.id" class="min-w-0">
            <h3 class="mb-1.5 text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
              {{ t(`about.shortcuts.groups.${group.id}`) }}
            </h3>
            <dl class="divide-y divide-border/50">
              <div v-for="item in group.items" :key="item.id" class="flex items-center justify-between gap-3 py-1.5">
                <dt class="min-w-0 text-xs">{{ t(`about.shortcuts.items.${item.id}`) }}</dt>
                <dd class="flex shrink-0 flex-wrap items-center justify-end gap-1">
                  <template v-for="(combo, i) in item.keys" :key="i">
                    <span v-if="i > 0" class="text-[10px] text-muted-foreground">{{ t('about.shortcuts.or') }}</span>
                    <kbd
                      v-for="k in combo"
                      :key="k"
                      class="inline-flex h-5 min-w-5 items-center justify-center rounded border border-border bg-muted px-1.5 font-mono text-[10px] text-foreground"
                    >{{ key(k) }}</kbd>
                  </template>
                </dd>
              </div>
            </dl>
          </section>
        </div>
      </Card>
    </div>
  </div>
</template>
