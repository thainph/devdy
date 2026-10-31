<script setup lang="ts">
/**
 * Controlling side of a remote desktop, in the sidebar footer spot the local
 * Remote Control dot (components/RemoteIndicator.vue) uses: colour = link state,
 * label = the controlled device. The popover holds the ways out — Files,
 * reconnect after an error, sign in again, sign out.
 *
 * `floating`: the bare dot pinned to a corner while the sidebar is hidden.
 * Connection problems also raise components/remote/RemoteDesktopAlert.vue, so
 * they are seen without opening this.
 */
import { ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { PopoverContent, PopoverPortal, PopoverRoot, PopoverTrigger } from 'radix-vue'
import { ArrowLeftRight, LogOut, MonitorSmartphone, RefreshCw } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { popoverContent } from '@/components/ui/calendarStyles'
import { useEscapeClose } from '@/composables/useEscapeClose'
import { useRemoteDesktopLink } from '@/composables/useRemoteDesktopLink'

defineProps<{ floating?: boolean }>()

const { t } = useI18n()
const link = useRemoteDesktopLink()
const { tone, connected, label, stateText, dotClass, canReconnect, needsSignIn } = link
const open = ref(false)
useEscapeClose(open)

function run(action: () => void) {
  open.value = false
  action()
}
</script>

<template>
  <PopoverRoot v-if="link.enabled" v-model:open="open">
    <PopoverTrigger as-child>
      <button
        type="button"
        class="flex min-w-0 items-center gap-1.5 rounded px-1 text-[10px] text-muted-foreground/70 hover:text-foreground"
        :class="floating && 'fixed bottom-3 left-3 z-[90] border border-border bg-background/95 p-1.5 shadow-lg backdrop-blur'"
        :title="`${t('remote.desktop.banner.controlling', { device: label })} · ${stateText}`"
        :aria-label="`${t('remote.desktop.banner.controlling', { device: label })} · ${stateText}`"
      >
        <span class="relative inline-flex h-2 w-2 shrink-0 rounded-full" :class="dotClass" />
        <span v-if="!floating" class="truncate">{{ label }}</span>
      </button>
    </PopoverTrigger>
    <PopoverPortal>
      <PopoverContent
        :side="floating ? 'right' : 'top'"
        align="end"
        :side-offset="6"
        :class="[popoverContent, 'w-64 space-y-2.5']"
      >
        <div class="space-y-1">
          <p class="flex items-center gap-1.5 text-xs font-semibold">
            <MonitorSmartphone class="h-3.5 w-3.5 shrink-0 text-primary" :stroke-width="2" />
            <span class="min-w-0 truncate">{{ t('remote.desktop.banner.controlling', { device: label }) }}</span>
          </p>
          <p
            class="flex items-center gap-1.5 text-[11px]"
            :class="tone === 'failed' ? 'text-red-600 dark:text-red-400' : 'text-muted-foreground'"
          >
            <span class="inline-flex h-2 w-2 shrink-0 rounded-full" :class="dotClass" />
            {{ stateText }}
          </p>
        </div>

        <Button
          v-if="connected"
          size="sm"
          variant="outline"
          class="w-full"
          :title="t('remote.desktop.banner.filesHint')"
          @click="run(link.openFiles)"
        >
          <ArrowLeftRight class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('remote.desktop.banner.files') }}
        </Button>
        <Button
          v-if="canReconnect && !needsSignIn"
          size="sm"
          variant="outline"
          class="w-full"
          @click="run(link.reconnect)"
        >
          <RefreshCw class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('remote.desktop.banner.reconnect') }}
        </Button>
        <Button v-if="needsSignIn" size="sm" class="w-full" @click="run(link.signOut)">
          {{ t('remote.desktop.banner.signInAgain') }}
        </Button>

        <button
          v-if="!needsSignIn"
          type="button"
          class="flex w-full items-center gap-1.5 text-[11px] text-muted-foreground hover:text-foreground"
          @click="run(link.signOut)"
        >
          <LogOut class="h-3 w-3" :stroke-width="1.75" />
          {{ t('remote.desktop.banner.signOut') }}
        </button>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
