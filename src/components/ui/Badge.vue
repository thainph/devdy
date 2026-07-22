<script setup lang="ts">
// Small status/label pill. `tone` covers the app's semantic colors; `neutral`
// is the default muted chip used for tags (engine, target, etc.).
import { computed, useAttrs } from 'vue'
import { cva, type VariantProps } from 'class-variance-authority'
import { cn } from '@/lib/utils'

defineOptions({ inheritAttrs: false })

// Solid-fill tones: a deep saturated -600 background with white text — high
// contrast and cleaner than the old translucent /15 + -400 text (which was
// low-contrast at these 9-10px sizes). One rule for every chip, app-wide.
const badge = cva(
  'inline-flex items-center gap-1 rounded border font-semibold whitespace-nowrap',
  {
    variants: {
      tone: {
        neutral: 'bg-slate-600 text-white border-slate-600',
        running: 'bg-blue-600 text-white border-blue-600',
        success: 'bg-emerald-600 text-white border-emerald-600',
        error: 'bg-red-600 text-white border-red-600',
        warning: 'bg-amber-600 text-white border-amber-600',
        info: 'bg-violet-600 text-white border-violet-600',
        primary: 'bg-indigo-600 text-white border-indigo-600',
      },
      size: {
        xs: 'px-1.5 py-0.5 text-[9px]',
        sm: 'px-1.5 py-0.5 text-[10px]',
      },
    },
    defaultVariants: { tone: 'neutral', size: 'sm' },
  },
)

type BadgeVariants = VariantProps<typeof badge>

const props = withDefaults(defineProps<{
  tone?: BadgeVariants['tone']
  size?: BadgeVariants['size']
}>(), {})

const attrs = useAttrs()
const classes = computed(() => cn(badge({ tone: props.tone, size: props.size }), attrs.class as string | undefined))
</script>

<template>
  <span v-bind="{ ...attrs, class: undefined }" :class="classes">
    <slot />
  </span>
</template>
