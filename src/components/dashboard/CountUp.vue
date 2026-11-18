<script setup lang="ts">
// A whole number that counts up on mount and eases to each new value — CSS only
// (a registered integer property, `.rt-count` in runtime.css); static under
// reduced motion. Screen readers get the plain number.
defineProps<{ value: number }>()
</script>

<template>
  <span class="tabular-nums">
    <span class="rt-count" :style="{ '--rt-n': Math.max(0, Math.round(value)) }" aria-hidden="true" />
    <span class="sr-only">{{ value }}</span>
  </span>
</template>
