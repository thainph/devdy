<script setup lang="ts">
/**
 * Setting → Thông báo, this phone's push notifications, laid out like the
 * other Setting screens: a switch to turn them on / off (or, where it cannot
 * work yet, why — iOS needs the app on the Home Screen first), a test send,
 * and a switch per kind for this device. A kind the desktop switched off is
 * off for every device, so its row says so and cannot be turned on here. An
 * older Host cannot store device switches: the desktop's are shown read-only.
 * Only shown when the Host advertises `push`.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, Send, Share } from 'lucide-vue-next'
import type { PushStore } from '../pushStore'

const props = defineProps<{ push: PushStore }>()

const { t } = useI18n()

const s = computed(() => props.push.state)
const ui = computed(() => props.push.ui())

const RUN_KINDS = ['pending', 'done', 'failed'] as const
// Focus reminders only from a Host that sends them.
const kinds = computed(() =>
  s.value.events?.focus === undefined ? RUN_KINDS : ([...RUN_KINDS, 'focus'] as const),
)
const switchable = computed(() => ui.value === 'on' || ui.value === 'off')
/** Per-device switches: subscribed, and a Host that can store them. */
const perDevice = computed(() => ui.value === 'on' && !!s.value.deviceEvents)

function toggle(): void {
  if (s.value.busy) return
  if (ui.value === 'on') void props.push.disable()
  else if (ui.value === 'off' && s.value.vapidKey) void props.push.enable()
}

const heading = 'mt-4 mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
const row = 'flex w-full items-center gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 text-left'
</script>

<template>
  <main class="h-full min-h-0 overflow-auto px-3 py-3 text-foreground">
    <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
      <div :class="row">
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-medium">{{ t('controller.push.receive') }}</p>
          <p class="text-[11px] text-muted-foreground">{{ t(`controller.push.state.${ui}`) }}</p>
        </div>
        <Loader2 v-if="s.busy" class="h-4 w-4 shrink-0 animate-spin text-muted-foreground" :stroke-width="2" />
        <button
          v-if="switchable"
          type="button"
          role="switch"
          :aria-checked="ui === 'on'"
          :aria-label="t('controller.push.receive')"
          :disabled="s.busy || (ui === 'off' && !s.vapidKey)"
          class="ui-switch relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 disabled:opacity-50 disabled:cursor-not-allowed"
          :class="ui === 'on' ? 'bg-success' : 'bg-muted'"
          @click="toggle"
        >
          <span
            class="ui-switch-knob inline-block h-5 w-5 rounded-full bg-white shadow transition-transform"
            :class="ui === 'on' ? 'translate-x-[22px]' : 'translate-x-0.5'"
          />
        </button>
      </div>
      <ol
        v-if="ui === 'needs_install'"
        class="list-decimal space-y-1 px-3 pt-1 pb-3 pl-8 text-[13px] text-foreground/70"
      >
        <li>
          {{ t('controller.push.install.share') }}
          <Share class="inline h-3.5 w-3.5 align-text-bottom" :stroke-width="2" />
        </li>
        <li>{{ t('controller.push.install.add') }}</li>
        <li>{{ t('controller.push.install.open') }}</li>
      </ol>
      <button
        v-if="ui === 'on'"
        type="button"
        :class="row"
        class="cursor-pointer text-sm font-medium disabled:opacity-50 disabled:cursor-not-allowed"
        :disabled="s.busy"
        @click="push.sendTest()"
      >
        <Send class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
        {{ t('controller.push.sendTest') }}
      </button>
    </div>

    <p v-if="s.error" class="mt-2 px-0.5 text-xs text-destructive">{{ t(`controller.push.errors.${s.error}`) }}</p>
    <p
      v-else-if="s.test"
      class="mt-2 px-0.5 text-xs"
      :class="s.test === 'sent' ? 'text-success' : 'text-destructive'"
    >
      {{ t(`controller.push.test.${s.test}`) }}
    </p>

    <template v-if="perDevice && s.events && s.deviceEvents">
      <h2 :class="heading">{{ t('controller.push.deviceTitle') }}</h2>
      <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
        <div v-for="k in kinds" :key="k" :class="row">
          <div class="min-w-0 flex-1">
            <p class="truncate text-sm font-medium">{{ t(`controller.push.kindRows.${k}`) }}</p>
            <p v-if="!s.events[k]" class="text-[11px] text-muted-foreground">{{ t('controller.push.offOnDesktop') }}</p>
          </div>
          <button
            type="button"
            role="switch"
            :aria-checked="s.events[k] && s.deviceEvents[k] !== false"
            :aria-label="t(`controller.push.kindRows.${k}`)"
            :disabled="s.busy || !s.events[k]"
            class="ui-switch relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 disabled:opacity-50 disabled:cursor-not-allowed"
            :class="s.events[k] && s.deviceEvents[k] !== false ? 'bg-success' : 'bg-muted'"
            @click="push.setKind(k, s.deviceEvents[k] === false)"
          >
            <span
              class="ui-switch-knob inline-block h-5 w-5 rounded-full bg-white shadow transition-transform"
              :class="s.events[k] && s.deviceEvents[k] !== false ? 'translate-x-[22px]' : 'translate-x-0.5'"
            />
          </button>
        </div>
      </div>
      <p class="mt-1.5 px-0.5 text-[11px] text-muted-foreground">{{ t('controller.push.deviceHint') }}</p>
    </template>
    <template v-else-if="switchable && s.events">
      <h2 :class="heading">{{ t('controller.push.sendsTitle') }}</h2>
      <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
        <div v-for="k in kinds" :key="k" :class="row">
          <span class="min-w-0 flex-1 truncate text-sm font-medium">{{ t(`controller.push.kindRows.${k}`) }}</span>
          <span class="shrink-0 text-xs" :class="s.events[k] ? 'text-success' : 'text-muted-foreground'">
            {{ s.events[k] ? t('controller.push.statusOn') : t('controller.push.statusOff') }}
          </span>
        </div>
      </div>
      <p class="mt-1.5 px-0.5 text-[11px] text-muted-foreground">{{ t('controller.push.kindsHint') }}</p>
    </template>

    <p v-if="switchable" class="mt-5 mb-2 text-center text-xs text-muted-foreground">{{ t('controller.push.privacy') }}</p>
  </main>
</template>
