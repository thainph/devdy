<script setup lang="ts">
// Manage extra GitHub logins the PR inbox treats as bots (on top of GitHub's
// own bot flag) — for service accounts that are plain "User"s on GitHub.
// Stored in localStorage via the prInbox store; logins are trimmed and
// compared case-insensitively.
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { usePrInboxStore } from '@/stores/prInbox'
import { Modal, Button, Input } from '@/components/ui'
import { Plus, X } from 'lucide-vue-next'

defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()
const store = usePrInboxStore()
const draft = ref('')

function add() {
  // Accept a pasted `@login` too.
  const login = draft.value.trim().replace(/^@/, '')
  if (!login) return
  store.addBotLogin(login)
  draft.value = ''
}
</script>

<template>
  <Modal :open="open" :title="t('prInbox.botAccounts.title')" size="sm" @close="emit('close')">
    <div class="flex flex-col gap-3 px-4 py-4">
      <p class="text-xs text-muted-foreground">{{ t('prInbox.botAccounts.hint') }}</p>
      <form class="flex items-center gap-2" @submit.prevent="add">
        <Input v-model="draft" :placeholder="t('prInbox.botAccounts.placeholder')" class="flex-1" />
        <Button type="submit" :disabled="!draft.trim()">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('prInbox.botAccounts.add') }}
        </Button>
      </form>
      <p v-if="store.botLogins.length === 0" class="text-xs text-muted-foreground/70">
        {{ t('prInbox.botAccounts.empty') }}
      </p>
      <ul v-else class="flex flex-col divide-y divide-border/60 rounded-md border border-border/60">
        <li v-for="login in store.botLogins" :key="login" class="flex items-center justify-between gap-2 px-3 py-1.5">
          <span class="font-mono text-xs truncate">{{ login }}</span>
          <Button
            variant="destructive-ghost"
            size="icon-sm"
            :title="t('prInbox.botAccounts.remove')"
            :aria-label="t('prInbox.botAccounts.remove')"
            @click="store.removeBotLogin(login)"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </Button>
        </li>
      </ul>
    </div>
    <template #footer>
      <Button variant="outline" @click="emit('close')">{{ t('common.close') }}</Button>
    </template>
  </Modal>
</template>
