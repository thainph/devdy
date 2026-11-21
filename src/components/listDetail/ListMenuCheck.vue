<script setup lang="ts">
// A checkable row of a sidebar menu (DropdownItem): a ✓ while `checked`, an
// optional colour dot (`dot`: a bg-* class, e.g. 'bg-destructive'), the label
// in the default slot and an optional count on the right. `radio` marks it as
// one choice of a group (role menuitemradio) instead of an on/off toggle.
import { Check } from 'lucide-vue-next'
import { DropdownItem } from '@/components/ui'

defineProps<{
  checked: boolean
  dot?: string
  count?: number | null
  radio?: boolean
}>()
</script>

<template>
  <DropdownItem :role="radio ? 'menuitemradio' : 'menuitemcheckbox'" :aria-checked="checked">
    <Check class="h-3.5 w-3.5 shrink-0 text-primary" :class="!checked && 'invisible'" :stroke-width="2.25" />
    <span v-if="dot" class="h-1.5 w-1.5 shrink-0 rounded-full" :class="dot" aria-hidden="true" />
    <span class="min-w-0 flex-1 truncate"><slot /></span>
    <span v-if="count != null" class="shrink-0 pl-3 text-[11px] tabular-nums text-muted-foreground">{{ count }}</span>
  </DropdownItem>
</template>
