<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { AppSelect } from '@/components/ui'
import { useI18n } from 'vue-i18n'
import { useAwsAccountsStore, type AwsProfileInfo } from '@/stores/awsAccounts'

// Simple profile picker: scans ~/.aws/config, lists the profiles found, and emits
// the selected profile name (v-model). Each profile already encodes one SSO role,
// so picking a profile is all that's needed. Emits `picked` with the full profile
// so the parent can auto-fill the label/region.
const props = defineProps<{ modelValue: string }>()
const emit = defineEmits<{
  'update:modelValue': [value: string]
  picked: [profile: AwsProfileInfo]
}>()

const { t } = useI18n()
const store = useAwsAccountsStore()

const profiles = ref<AwsProfileInfo[]>([])
const loading = ref(false)
const error = ref<string | null>(null)

function caption(p: AwsProfileInfo): string {
  if (p.isSso) {
    const parts = [p.ssoRoleName ?? 'SSO']
    if (p.ssoAccountId) parts.push(p.ssoAccountId)
    return parts.join(' · ')
  }
  return t('settings.aws.profileNonSso')
}

const options = computed(() =>
  profiles.value.map((p) => ({ value: p.name, label: p.name, description: caption(p) })),
)
const selected = computed(() => profiles.value.find((p) => p.name === props.modelValue) ?? null)

async function load() {
  loading.value = true
  error.value = null
  try {
    profiles.value = await store.listProfiles()
  } catch (e) {
    error.value = String(e)
  } finally {
    loading.value = false
  }
}

function onSelect(name: string) {
  emit('update:modelValue', name)
  const p = profiles.value.find((x) => x.name === name)
  if (p) emit('picked', p)
}

onMounted(load)
</script>

<template>
  <div class="space-y-1">
    <label class="text-[11px] font-medium text-muted-foreground">{{ t('settings.aws.profileLabel') }}</label>
    <AppSelect
      size="sm"
      :model-value="modelValue"
      :options="options"
      :placeholder="loading ? t('common.loading') : t('settings.aws.profileSelectPlaceholder')"
      :disabled="loading"
      @update:model-value="onSelect"
    />
    <p v-if="selected" class="text-[11px] text-muted-foreground">
      <span v-if="selected.isSso">
        {{ t('settings.aws.ssoRoleLabel') }}:
        <span class="text-foreground">{{ selected.ssoRoleName ?? '—' }}</span>
        <template v-if="selected.ssoAccountId"> · {{ selected.ssoAccountId }}</template>
      </span>
      <template v-if="selected.region"> · {{ selected.region }}</template>
    </p>
    <p v-else-if="!loading && !profiles.length" class="text-[11px] text-muted-foreground">
      {{ t('settings.aws.profileNoneFound') }}
    </p>
    <p v-if="error" class="text-[11px] text-destructive">{{ error }}</p>
  </div>
</template>
