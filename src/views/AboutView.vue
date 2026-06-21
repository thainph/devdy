<script setup lang="ts">
import { Globe } from 'lucide-vue-next'
import { Badge } from '@/components/ui'
</script>

<template>
  <div class="flex flex-col h-full">
    <div class="flex items-center px-6 h-13 border-b border-border/60 shrink-0">
      <h1 class="text-sm font-semibold">About</h1>
    </div>

    <div class="flex-1 flex items-center justify-center p-6">
      <div class="flex flex-col items-center text-center max-w-xs">
        <!-- Logo -->
        <img src="/logo.png" alt="Devdy" class="h-14 w-14 rounded-2xl mb-5" />

        <h2 class="text-xl font-bold tracking-tight">Devdy</h2>
        <p class="text-sm text-muted-foreground mt-1">AI coding agent for developers</p>
        <Badge class="mt-2 px-2 py-0.5 text-[10px] font-mono font-medium">
          v0.1.0
        </Badge>

        <p class="mt-6 text-xs text-muted-foreground leading-relaxed">
          Manage AI skills centrally and automate GitHub issue and pull request analysis
          using claude or codex — all from a single desktop app.
        </p>

        <div class="mt-8 flex items-center gap-3 text-xs text-muted-foreground">
          <span class="flex items-center gap-1">
            <Globe class="h-3 w-3" :stroke-width="1.5" />
            Tauri 2
          </span>
          <span class="h-1 w-1 rounded-full bg-border" />
          <span>Vue 3</span>
          <span class="h-1 w-1 rounded-full bg-border" />
          <span>Rust</span>
        </div>
      </div>
    </div>
  </div>
</template>
