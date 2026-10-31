<script setup lang="ts">
// GitHub / GitLab / AWS / Azure tab of a project: every configured account with
// an on/off switch (like the Deploy servers list) and a "make default" action.
// Sessions may use any enabled account; commands without DEVDY_GIT_ACCOUNT /
// DEVDY_AWS_ACCOUNT / DEVDY_AZURE_ACCOUNT use the default.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'
import { Badge, Card } from '@/components/ui'
import { Cloud, CloudCog, Github, Gitlab, Loader2 } from 'lucide-vue-next'

export type AccountProvider = 'github' | 'gitlab' | 'aws' | 'azure'

export interface AccountRow {
  id: string
  label: string
  username: string | null
  /** Secondary line, e.g. scopes (GitHub), host (GitLab), region (AWS). */
  detail: string | null
  /** Amber marker after the detail line, e.g. Azure "not signed in". */
  warning?: string | null
}

const props = defineProps<{
  provider: AccountProvider
  accounts: AccountRow[]
  enabledIds: string[]
  defaultId: string | null
  /** Account id currently being toggled / made default, or null. */
  busy: string | null
}>()

const emit = defineEmits<{
  toggle: [account: AccountRow, enabled: boolean]
  makeDefault: [account: AccountRow]
}>()

const { t } = useI18n()

const PROVIDERS = {
  github: { icon: Github, title: 'githubAccounts', hint: 'gitAccountsHint', empty: 'noGithubAccounts', envVar: 'DEVDY_GIT_ACCOUNT', settings: '/settings' },
  gitlab: { icon: Gitlab, title: 'gitlabAccounts', hint: 'gitAccountsHint', empty: 'noGitlabAccounts', envVar: 'DEVDY_GIT_ACCOUNT', settings: '/settings' },
  aws: { icon: Cloud, title: 'awsAccounts', hint: 'awsAccountHint', empty: 'noAwsAccounts', envVar: 'DEVDY_AWS_ACCOUNT', settings: '/settings' },
  azure: { icon: CloudCog, title: 'azureAccounts', hint: 'azureAccountHint', empty: 'noAzureAccounts', envVar: 'DEVDY_AZURE_ACCOUNT', settings: '/settings?section=azure' },
} as const

const config = computed(() => PROVIDERS[props.provider])
const enabled = computed(() => new Set(props.enabledIds))
const enabledCount = computed(() => props.accounts.filter(a => enabled.value.has(a.id)).length)
</script>

<template>
  <Card>
    <template #header>
      <component :is="config.icon" class="h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.5" />
      <span class="text-xs font-semibold">{{ t(`projectDetail.${config.title}`) }}</span>
      <span
        v-if="enabledCount > 0"
        class="flex h-4 min-w-4 items-center justify-center rounded-full bg-muted px-1 text-[10px] font-medium text-muted-foreground leading-none"
      >{{ enabledCount }}</span>
      <RouterLink :to="config.settings" class="ml-auto text-[11px] text-primary hover:underline">{{ t('projectDetail.manage') }}</RouterLink>
    </template>

    <p class="px-4 pt-3 text-[11px] text-muted-foreground leading-relaxed">
      {{ t(`projectDetail.${config.hint}`) }}
      <code class="text-primary">{{ config.envVar }}=&lt;label&gt;</code>.
    </p>

    <div v-if="accounts.length === 0" class="px-4 py-5 text-center text-xs text-muted-foreground">
      {{ t(`projectDetail.${config.empty}`) }}
      <RouterLink :to="config.settings" class="text-primary hover:underline">{{ t('projectDetail.addInSettings') }}</RouterLink>
      {{ t('projectDetail.toLinkHere') }}
    </div>

    <div v-else class="mt-2 divide-y divide-border/50 border-t border-border/50">
      <div
        v-for="account in accounts"
        :key="account.id"
        class="flex items-center gap-3 px-4 py-3"
      >
        <div class="flex-1 min-w-0">
          <div class="flex items-center gap-1.5 flex-wrap">
            <p class="text-xs font-medium truncate">{{ account.label }}</p>
            <Badge
              v-if="account.id === defaultId"
              tone="primary"
              size="xs"
              class="shrink-0 uppercase tracking-wide"
            >{{ t('projectDetail.defaultAccount') }}</Badge>
            <button
              v-else-if="enabled.has(account.id)"
              type="button"
              class="text-[10px] text-primary hover:underline disabled:opacity-50"
              :disabled="busy !== null"
              @click="emit('makeDefault', account)"
            >{{ t('projectDetail.makeDefault') }}</button>
          </div>
          <p v-if="account.username || account.detail || account.warning" class="text-[10px] text-muted-foreground font-mono truncate mt-0.5">
            <span v-if="account.username">@{{ account.username }}</span>
            <span v-if="account.username && account.detail"> · </span>
            <span v-if="account.detail">{{ account.detail }}</span>
            <span v-if="account.warning" class="text-amber-500 font-sans">
              <template v-if="account.username || account.detail"> · </template>{{ account.warning }}
            </span>
          </p>
        </div>
        <button
          type="button"
          role="switch"
          :aria-checked="enabled.has(account.id)"
          :disabled="busy !== null"
          class="relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 disabled:cursor-not-allowed disabled:opacity-50"
          :class="enabled.has(account.id) ? 'bg-primary' : 'bg-muted'"
          :title="enabled.has(account.id) ? t('projectDetail.disableForProject') : t('projectDetail.enableForProject')"
          @click="emit('toggle', account, !enabled.has(account.id))"
        >
          <span
            class="inline-flex h-4 w-4 transform items-center justify-center rounded-full bg-white shadow transition-transform"
            :class="enabled.has(account.id) ? 'translate-x-6' : 'translate-x-1'"
          >
            <Loader2 v-if="busy === account.id" class="h-3 w-3 animate-spin text-primary" :stroke-width="2.5" />
          </span>
        </button>
      </div>
    </div>
  </Card>
</template>
