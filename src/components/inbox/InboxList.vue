<script setup lang="ts">
// Frame of one inbox list group (PR Inbox, GitHub Mentions): a single card
// whose InboxCard rows are divided by hairlines — rows, not separate cards.
</script>

<template>
  <div class="ui-card glass flex flex-col divide-y divide-border/50 overflow-hidden rounded-lg border border-border/70">
    <slot />
  </div>
</template>
