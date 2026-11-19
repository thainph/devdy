<script setup lang="ts">
// A Runtime Dashboard tile: the shared Card, opaque with a border tinted from
// `primary` (`.rt-card`, runtime.css), an icon + title header and an optional
// right-hand aside. `compact` = the short one-row strip on top of the
// Dashboard: fixed height, tighter padding and a smaller header.
import type { Component } from 'vue'
import { Card } from '@/components/ui'

withDefaults(defineProps<{ title: string; icon: Component; tone: 'primary' | 'warning' | 'success' | 'muted'; compact?: boolean }>(), { compact: false })

const TONE = {
  primary: 'bg-primary/15 text-primary',
  warning: 'bg-warning/15 text-warning',
  success: 'bg-success/15 text-success',
  muted: 'bg-muted text-muted-foreground',
} as const
</script>

<template>
  <Card
    class="rt-card min-w-0"
    :class="compact && 'h-[132px]'"
    :body-class="compact ? 'flex h-full min-h-0 flex-col gap-1.5 px-3 py-2' : 'flex h-full flex-col gap-3 p-4'"
    role="region"
    :aria-label="title"
  >
    <div class="flex min-w-0 items-center font-semibold text-muted-foreground" :class="compact ? 'gap-1.5 text-[11px]' : 'gap-2 text-xs'">
      <span class="grid shrink-0 place-items-center rounded-md" :class="[TONE[tone], compact ? 'h-[18px] w-[18px]' : 'h-6 w-6']">
        <component :is="icon" :class="compact ? 'h-3 w-3' : 'h-3.5 w-3.5'" :stroke-width="2" aria-hidden="true" />
      </span>
      <span class="min-w-0 truncate">{{ title }}</span>
      <slot name="aside" />
    </div>
    <slot />
  </Card>
</template>
