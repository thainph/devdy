<script setup lang="ts">
// A Runtime Dashboard tile: the shared Card, opaque with a border tinted from
// `primary` (`.rt-card`, runtime.css), an icon + title header and an optional
// right-hand aside.
import type { Component } from 'vue'
import { Card } from '@/components/ui'

defineProps<{ title: string; icon: Component; tone: 'primary' | 'warning' | 'success' | 'muted' }>()

const TONE = {
  primary: 'bg-primary/15 text-primary',
  warning: 'bg-warning/15 text-warning',
  success: 'bg-success/15 text-success',
  muted: 'bg-muted text-muted-foreground',
} as const
</script>

<template>
  <Card class="rt-card min-w-0" body-class="flex h-full flex-col gap-3 p-4" role="region" :aria-label="title">
    <div class="flex min-w-0 items-center gap-2 text-xs font-semibold text-muted-foreground">
      <span class="grid h-6 w-6 shrink-0 place-items-center rounded-md" :class="TONE[tone]">
        <component :is="icon" class="h-3.5 w-3.5" :stroke-width="2" aria-hidden="true" />
      </span>
      <span class="min-w-0 truncate">{{ title }}</span>
      <slot name="aside" />
    </div>
    <slot />
  </Card>
</template>
