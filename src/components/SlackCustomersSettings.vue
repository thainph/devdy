<script setup lang="ts">
// Settings → "Slack customers": the Slack users marked as customers from the
// Slack threads window (a thread whose latest human message comes from one of
// them is "waiting on you" on the Work Dashboard). Lists them with their
// workspace, lets the user change the project they belong to, or unmark them.
import { onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { RefreshCw, UserCheck, UserX } from 'lucide-vue-next'
import { Button, Card } from '@/components/ui'
import ProjectSelect from '@/components/ProjectSelect.vue'
import SlackAvatar from '@/components/slack/SlackAvatar.vue'
import { useToast } from '@/composables/useToast'
import { useSlackCustomersStore, type SlackCustomer } from '@/stores/slackCustomers'

const { t } = useI18n()
const { toast } = useToast()
const store = useSlackCustomersStore()

async function setProject(c: SlackCustomer, projectId: string | null) {
  try {
    await store.mark(c.teamId, c.userId, projectId, c.note)
  } catch (e) {
    toast.error(String(e))
  }
}

async function unmark(c: SlackCustomer) {
  try {
    await store.unmark(c.teamId, c.userId)
    toast.success(t('slackThread.customer.unmarked', { name: c.displayName }))
  } catch (e) {
    toast.error(String(e))
  }
}

onMounted(() => {
  void store.start()
  void store.load()
})
</script>

<template>
  <Card body-class="p-4 space-y-3">
    <template #header>
      <UserCheck class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="flex-1 text-xs font-semibold">{{ t('settings.slackCustomers.title') }}</span>
      <Button
        variant="ghost"
        size="icon-sm"
        :title="t('settings.slackCustomers.refresh')"
        :aria-label="t('settings.slackCustomers.refresh')"
        @click="store.load()"
      >
        <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </template>

    <p class="text-[11px] text-muted-foreground">{{ t('settings.slackCustomers.help') }}</p>

    <p v-if="store.error" class="text-[11px] text-destructive">{{ store.error }}</p>

    <div v-else-if="!store.loaded" class="h-16 animate-pulse rounded-md border border-border bg-card" />

    <p
      v-else-if="store.customers.length === 0"
      class="rounded-md border border-dashed border-border/70 px-3 py-4 text-center text-[11px] text-muted-foreground"
    >
      {{ t('settings.slackCustomers.empty') }}
    </p>

    <ul v-else class="divide-y divide-border/50 rounded-md border border-border/60">
      <li
        v-for="c in store.customers"
        :key="`${c.teamId}/${c.userId}`"
        class="flex items-center gap-2.5 px-3 py-2"
      >
        <SlackAvatar :path="c.avatarPath" :name="c.displayName" :tone-key="c.userId" :size="24" />
        <div class="min-w-0 flex-1">
          <p class="truncate text-xs font-medium text-foreground" :title="c.realName ?? undefined">
            {{ c.displayName }}
          </p>
          <p class="truncate text-[11px] text-muted-foreground">
            {{ c.workspace ?? c.teamId }}<template v-if="c.note"> · {{ c.note }}</template>
          </p>
        </div>
        <ProjectSelect
          :model-value="c.projectId"
          size="xs"
          class="w-40"
          @update:model-value="setProject(c, $event)"
        />
        <Button
          variant="destructive-ghost"
          size="icon-sm"
          :title="t('slackThread.customer.unmarkTitle', { name: c.displayName })"
          :aria-label="t('slackThread.customer.unmarkTitle', { name: c.displayName })"
          @click="unmark(c)"
        >
          <UserX class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </li>
    </ul>
  </Card>
</template>
