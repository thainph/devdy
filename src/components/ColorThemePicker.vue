<script setup lang="ts">
/**
 * Color palette picker: grouped grid of preview tiles. Each tile is a
 * `.theme-scope` carrying its own `data-theme` (and `.dark` when the app is in
 * dark mode), so it paints that palette's tokens rather than the active one.
 */
import { onMounted, onUnmounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check } from 'lucide-vue-next'
import { COLOR_THEME_GROUPS, colorThemeLabelKey, colorThemesIn } from '@/lib/colorThemes'

const model = defineModel<string>({ required: true })

const { t } = useI18n()

const isDark = ref(document.documentElement.classList.contains('dark'))
let observer: MutationObserver | null = null
onMounted(() => {
  observer = new MutationObserver(() => {
    isDark.value = document.documentElement.classList.contains('dark')
  })
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] })
})
onUnmounted(() => observer?.disconnect())
</script>

<template>
  <div class="space-y-3">
    <div v-for="group in COLOR_THEME_GROUPS" :key="group" class="space-y-1.5">
      <p class="text-[10px] font-medium uppercase tracking-wider text-muted-foreground/80">
        {{ t(`settings.general.colorGroup.${group}`) }}
      </p>
      <div class="grid grid-cols-3 gap-1.5">
        <button
          v-for="theme in colorThemesIn(group)"
          :key="theme.id"
          type="button"
          class="flex flex-col gap-1 rounded-md border p-1 text-left transition-colors cursor-pointer"
          :class="model === theme.id ? 'border-primary ring-1 ring-primary' : 'border-border hover:border-muted-foreground/40'"
          :aria-pressed="model === theme.id"
          :title="t(colorThemeLabelKey(theme.id))"
          @click="model = theme.id"
        >
          <span
            :data-theme="theme.id === 'default' ? undefined : theme.id"
            class="theme-scope flex h-10 overflow-hidden rounded border border-border bg-background"
            :class="{ dark: isDark }"
          >
            <span class="w-3 shrink-0 border-r border-border bg-sidebar" />
            <span class="flex min-w-0 flex-1 flex-col justify-center gap-1 px-1.5">
              <span class="h-1.5 w-3/4 rounded-full bg-foreground/70" />
              <span class="h-1.5 w-1/2 rounded-full bg-muted-foreground/50" />
            </span>
            <span class="flex shrink-0 items-center gap-0.5 pr-1.5">
              <span class="h-3 w-3 rounded-full bg-primary" />
              <span class="h-3 w-3 rounded-full border border-border bg-accent" />
            </span>
          </span>
          <span class="flex items-center gap-1 px-0.5">
            <span class="min-w-0 flex-1 truncate text-[11px]">{{ t(colorThemeLabelKey(theme.id)) }}</span>
            <Check v-if="model === theme.id" class="h-3 w-3 shrink-0 text-primary" :stroke-width="2.5" />
          </span>
        </button>
      </div>
    </div>
  </div>
</template>
