<script setup lang="ts">
// The divider between groups of menu items. One component so every menu — ⋯
// dropdown or right-click context menu — draws it with the same weight and
// spacing instead of each host inlining its own <div class="my-1 …" />.
</script>

<template>
  <div class="my-1 h-px bg-border" aria-hidden="true" />
</template>
