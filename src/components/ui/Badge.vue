<script setup lang="ts">
// Small status/label pill. `tone` covers the app's semantic colors; `neutral`
// is the default muted chip used for tags (engine, target, etc.).
import { computed, useAttrs } from 'vue'
import { cva, type VariantProps } from 'class-variance-authority'
import { cn } from '@/lib/utils'

defineOptions({ inheritAttrs: false })

const badge = cva(
  'inline-flex items-center gap-1 rounded border font-medium',
  {
    variants: {
      tone: {
        neutral: 'bg-muted text-muted-foreground border-border/60',
        running: 'bg-blue-500/15 text-blue-400 border-blue-500/20',
        success: 'bg-emerald-500/15 text-emerald-400 border-emerald-500/20',
        error: 'bg-red-500/15 text-red-400 border-red-500/20',
        warning: 'bg-amber-500/15 text-amber-400 border-amber-500/20',
        info: 'bg-violet-500/15 text-violet-400 border-violet-500/20',
        primary: 'bg-primary/10 text-primary border-primary/20',
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
