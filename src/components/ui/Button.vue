<script setup lang="ts">
// Shared button. Centralizes the many ad-hoc primary/outline/ghost button class
// combos scattered across the app so sizing + hover stay consistent everywhere.
// Pass any extra classes via `class` — they're tailwind-merged over the variant.
import { computed, useAttrs } from 'vue'
import { cva, type VariantProps } from 'class-variance-authority'
import { cn } from '@/lib/utils'

defineOptions({ inheritAttrs: false })

const button = cva(
  'inline-flex items-center justify-center whitespace-nowrap rounded-md font-medium transition cursor-pointer select-none focus:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-50 disabled:pointer-events-none',
  {
    variants: {
      variant: {
        primary: 'bg-primary text-primary-foreground hover:opacity-90',
        outline: 'border border-border bg-secondary/60 text-foreground shadow-sm hover:bg-accent hover:text-accent-foreground',
        ghost: 'text-muted-foreground hover:bg-accent hover:text-foreground',
        destructive: 'border border-destructive/50 text-destructive hover:bg-destructive/10',
        'destructive-ghost': 'text-muted-foreground/60 hover:text-destructive hover:bg-destructive/10',
        subtle: 'bg-muted text-muted-foreground border border-border/60',
      },
      size: {
        xs: 'h-7 gap-1 px-2.5 text-[10px]',
        sm: 'h-8 gap-1.5 px-3 text-xs',
        md: 'h-9 gap-2 px-4 text-sm',
        icon: 'h-8 w-8',
        'icon-sm': 'h-7 w-7',
        'icon-lg': 'h-9 w-9',
      },
    },
    defaultVariants: { variant: 'primary', size: 'sm' },
  },
)

type ButtonVariants = VariantProps<typeof button>

const props = withDefaults(defineProps<{
  variant?: ButtonVariants['variant']
  size?: ButtonVariants['size']
  type?: 'button' | 'submit' | 'reset'
  disabled?: boolean
}>(), {
  type: 'button',
  disabled: false,
})

const attrs = useAttrs()
const classes = computed(() =>
  cn(button({ variant: props.variant, size: props.size }), attrs.class as string | undefined),
)
</script>

<template>
  <button
    v-bind="{ ...attrs, class: undefined }"
    :type="type"
    :disabled="disabled"
    :class="classes"
  >
    <slot />
  </button>
</template>
