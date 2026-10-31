<script setup lang="ts">
/** Stand-in for the Todo / Note / Project tabs until their views land. */
import { useI18n } from 'vue-i18n'
import { Hourglass } from 'lucide-vue-next'

defineProps<{ title: string }>()

const { t } = useI18n()
</script>

<template>
  <div class="flex h-full min-h-0 flex-col bg-background text-foreground">
    <header class="shrink-0 flex items-center px-3 py-2.5 border-b border-border">
      <h1 class="text-sm font-medium">{{ title }}</h1>
    </header>
    <main class="flex-1 min-h-0 flex flex-col items-center justify-center gap-2 px-6 text-center">
      <Hourglass class="h-6 w-6 text-foreground/30" :stroke-width="1.75" />
      <p class="text-sm font-medium">{{ t('controller.placeholder.title') }}</p>
      <p class="text-xs text-foreground/50">{{ t('controller.placeholder.body') }}</p>
    </main>
  </div>
</template>
