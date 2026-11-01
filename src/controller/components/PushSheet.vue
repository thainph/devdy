<script setup lang="ts">
/**
 * Bottom sheet for this phone's push notifications: turn them on (or learn how
 * to — iOS needs the app on the Home Screen first), see whether they are on,
 * send a test, turn them off. Only shown when the Host advertises `push`.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui'
import { Loader2, Share } from 'lucide-vue-next'
import type { PushStore } from '../pushStore'

const props = defineProps<{ push: PushStore }>()
const emit = defineEmits<{ close: [] }>()

const { t } = useI18n()

const s = computed(() => props.push.state)
const ui = computed(() => props.push.ui())

/** What the desktop sends at all (its switches). */
const kinds = computed(() => {
  const e = s.value.events
  if (!e) return ''
  return (['pending', 'done', 'failed'] as const)
    .filter((k) => e[k])
    .map((k) => t(`controller.push.kinds.${k}`))
    .join(' · ')
})
</script>

<template>
  <div class="absolute inset-0 z-20 flex flex-col justify-end">
    <div class="absolute inset-0 bg-black/50" @click="emit('close')" />
    <div
      class="relative rounded-t-2xl border-t border-border bg-card px-4 pt-2.5 pb-[max(1.25rem,env(safe-area-inset-bottom))] shadow-2xl"
    >
      <div class="mx-auto mb-3 h-1.5 w-10 rounded-full bg-foreground/20" />
      <p class="mb-2 text-sm font-semibold">{{ t('controller.push.title') }}</p>

      <p class="text-sm text-foreground/80">{{ t(`controller.push.state.${ui}`) }}</p>

      <ol
        v-if="ui === 'needs_install'"
        class="mt-2 list-decimal space-y-1 pl-5 text-[13px] text-foreground/70"
      >
        <li>
          {{ t('controller.push.install.share') }}
          <Share class="inline h-3.5 w-3.5 align-text-bottom" :stroke-width="2" />
        </li>
        <li>{{ t('controller.push.install.add') }}</li>
        <li>{{ t('controller.push.install.open') }}</li>
      </ol>

      <p v-if="ui === 'on' && kinds" class="mt-1 text-[11px] text-muted-foreground">
        {{ t('controller.push.sends', { kinds }) }}
      </p>
      <p v-if="ui === 'on' || ui === 'off'" class="mt-1 text-[11px] text-muted-foreground">
        {{ t('controller.push.privacy') }}
      </p>

      <p v-if="s.error" class="mt-2 text-xs text-destructive">{{ t(`controller.push.errors.${s.error}`) }}</p>
      <p
        v-else-if="s.test"
        class="mt-2 text-xs"
        :class="s.test === 'sent' ? 'text-success' : 'text-destructive'"
      >
        {{ t(`controller.push.test.${s.test}`) }}
      </p>

      <div class="mt-4 grid gap-2" :class="ui === 'on' ? 'grid-cols-2' : 'grid-cols-1'">
        <template v-if="ui === 'off'">
          <Button size="md" :disabled="s.busy || !s.vapidKey" @click="push.enable()">
            <Loader2 v-if="s.busy" class="h-4 w-4 animate-spin" :stroke-width="2" />
            {{ t('controller.push.enable') }}
          </Button>
        </template>
        <template v-else-if="ui === 'on'">
          <Button size="md" variant="outline" :disabled="s.busy" @click="push.sendTest()">
            <Loader2 v-if="s.busy" class="h-4 w-4 animate-spin" :stroke-width="2" />
            {{ t('controller.push.sendTest') }}
          </Button>
          <Button size="md" variant="outline" :disabled="s.busy" @click="push.disable()">
            {{ t('controller.push.disable') }}
          </Button>
        </template>
        <Button size="md" variant="ghost" class="col-span-full" @click="emit('close')">{{ t('controller.push.close') }}</Button>
      </div>
    </div>
  </div>
</template>
