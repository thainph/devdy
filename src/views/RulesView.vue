<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useRulesStore, type Rule } from '@/stores/rules'
import { open, save } from '@tauri-apps/plugin-dialog'
import { Button, Card, Badge } from '@/components/ui'
import { Plus, Upload, Download, Pencil, Trash2, ScrollText, CalendarDays } from 'lucide-vue-next'

const router = useRouter()
const store = useRulesStore()
const deletingId = ref<string | null>(null)
const importing = ref(false)

onMounted(() => store.fetchRules())

const targetLabel: Record<string, string> = { claude: 'Claude', codex: 'Codex', both: 'Both' }

async function handleDelete(rule: Rule) {
  if (!confirm(`Delete rule "${rule.name}"? It will also be removed from every project it's applied to.`)) return
  deletingId.value = rule.id
  try {
    await store.deleteRule(rule.id)
  } catch (e) {
    alert(String(e))
  } finally {
    deletingId.value = null
  }
}

async function handleImport() {
  const selected = await open({
    multiple: false,
    filters: [{ name: 'Markdown', extensions: ['md', 'markdown'] }],
  })
  if (!selected) return
  importing.value = true
  try {
    await store.importRule(selected as string)
  } catch (e) {
    alert(String(e))
  } finally {
    importing.value = false
  }
}

async function handleExport(rule: Rule) {
  const destPath = await save({
    defaultPath: `${rule.name}.md`,
    filters: [{ name: 'Markdown', extensions: ['md'] }],
  })
  if (!destPath) return
  try {
    await store.exportRule(rule.id, destPath)
  } catch (e) {
    alert(String(e))
  }
}

function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' })
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Page header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-sm font-semibold">Rules</h1>
        <span
          v-if="!store.loading && store.rules.length > 0"
          class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
        >
          {{ store.rules.length }}
        </span>
      </div>
      <div class="flex items-center gap-2">
        <Button
          variant="outline"
          :disabled="importing"
          @click="handleImport"
        >
          <Upload class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ importing ? 'Importing…' : 'Import .md' }}
        </Button>
        <Button
          @click="router.push('/rules/new')"
        >
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          New Rule
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 overflow-auto p-6">
      <!-- Loading skeleton -->
      <div v-if="store.loading" class="grid grid-cols-2 xl:grid-cols-3 gap-3">
        <div v-for="i in 5" :key="i" class="h-27 rounded-lg border border-border bg-card animate-pulse" />
      </div>

      <!-- Error -->
      <div v-else-if="store.error" class="p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <!-- Empty state -->
      <div v-else-if="store.rules.length === 0" class="flex flex-col items-center justify-center h-full min-h-80 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <ScrollText class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">No rules yet</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-56">Create a rule / convention for Claude (.claude/rules) or Codex (AGENTS.md)</p>
        <Button
          class="mt-4"
          @click="router.push('/rules/new')"
        >
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          New Rule
        </Button>
      </div>

      <!-- Cards grid -->
      <div v-else class="grid grid-cols-2 xl:grid-cols-3 gap-3">
        <Card
          v-for="rule in store.rules"
          :key="rule.id"
          class="group relative flex flex-col transition-all duration-150 cursor-pointer hover:border-primary/40 hover:shadow-[0_1px_3px_0_rgb(0_0_0/0.08)] hover:-translate-y-0.5"
          body-class="flex flex-col flex-1 p-4"
          @click="router.push(`/rules/${rule.id}/edit`)"
        >
          <!-- hover accent line -->
          <span class="absolute inset-x-0 top-0 h-0.5 origin-left scale-x-0 bg-linear-to-r from-primary to-primary/30 transition-transform duration-200 group-hover:scale-x-100" />

          <!-- Icon + name/desc -->
          <div class="flex items-start gap-3 mb-3">
            <div class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/15 text-primary transition-colors group-hover:bg-primary/15 group-hover:border-primary/25">
              <ScrollText class="h-4.5 w-4.5" :stroke-width="1.75" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-1.5">
                <p class="text-sm font-semibold font-mono truncate leading-tight">{{ rule.name }}</p>
                <Badge size="xs" class="shrink-0 uppercase tracking-wide">
                  {{ targetLabel[rule.target] }}
                </Badge>
              </div>
              <p class="text-xs text-muted-foreground mt-1 line-clamp-2 leading-relaxed">{{ rule.description }}</p>
            </div>
          </div>

          <!-- Footer -->
          <div class="flex items-center justify-between pt-2.5 border-t border-border/60 mt-auto">
            <div class="flex items-center gap-1 text-[10px] text-muted-foreground/60">
              <CalendarDays class="h-3 w-3" :stroke-width="1.5" />
              <span>{{ formatDate(rule.updated_at) }}</span>
            </div>
            <!-- Actions on hover -->
            <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity" @click.stop>
              <Button
                variant="ghost"
                size="icon-sm"
                title="Export .md"
                @click="handleExport(rule)"
              >
                <Download class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                title="Edit"
                @click="router.push(`/rules/${rule.id}/edit`)"
              >
                <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
              <Button
                variant="destructive-ghost"
                size="icon-sm"
                title="Delete"
                :disabled="deletingId === rule.id"
                @click="handleDelete(rule)"
              >
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  </div>
</template>
