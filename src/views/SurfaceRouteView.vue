<script setup lang="ts">
// Route component of every surface tab (/pr-inbox, /calendar, /todos, /file…).
// It renders nothing: App.vue keeps one SurfaceHost per open surface tab mounted
// beside the RouterView and shows the routed one, so each tab keeps its own
// state (scroll, selection, an unsaved edit) and is torn down when it closes.
// The route itself exists for the URL, history and the window/tab guard.
defineOptions({ name: 'SurfaceRouteView' })
</script>

<template>
  <div class="hidden" aria-hidden="true" />
</template>
