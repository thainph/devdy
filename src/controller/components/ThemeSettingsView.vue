<script setup lang="ts">
/**
 * Setting → Giao diện: this phone's look — light / dark / system, either
 * following the desktop or picked here; one row per choice, like the other
 * Setting screens. The choice is stored on this device only (see
 * ../appearance.ts) — nothing goes to the Host.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check } from 'lucide-vue-next'
import { FOLLOW_HOST, THEME_MODES } from '@/lib/controllerAppearance'
import { appearanceOverride, hostAppearance, setAppearanceOverride } from '../appearance'

const { t } = useI18n()

function modeLabel(mode: string): string {
  const key = { light: 'themeLight', dark: 'themeDark', system: 'themeSystem' }[mode]
  return key ? t(`settings.general.${key}`) : mode
}

/** "Same as desktop (Dark)" once the Host's mode is known. */
function followLabel(value: string | undefined): string {
  return value ? t('controller.theme.followHostValue', { value: modeLabel(value) }) : t('controller.theme.followHost')
}

const modeOptions = computed(() => [
  { value: FOLLOW_HOST, label: followLabel(hostAppearance.value?.theme) },
  ...THEME_MODES.map((m) => ({ value: m, label: modeLabel(m) })),
])

function setMode(theme: string): void {
  setAppearanceOverride({ theme })
}
</script>

<template>
  <main class="h-full min-h-0 overflow-auto px-3 py-3 text-foreground">
    <h2 class="mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
      {{ t('controller.theme.mode') }}
    </h2>
    <div class="ui-card overflow-hidden rounded-xl border border-border bg-card" role="radiogroup">
      <button
        v-for="o in modeOptions"
        :key="o.value"
        type="button"
        role="radio"
        :aria-checked="appearanceOverride.theme === o.value"
        class="flex w-full cursor-pointer items-center gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 text-left text-sm font-medium"
        @click="setMode(o.value)"
      >
        <span class="min-w-0 flex-1 truncate">{{ o.label }}</span>
        <Check v-if="appearanceOverride.theme === o.value" class="h-4 w-4 shrink-0 text-primary" :stroke-width="2.5" />
      </button>
    </div>
    <p class="mt-5 mb-2 text-center text-xs text-muted-foreground">{{ t('controller.theme.hint') }}</p>
  </main>
</template>
