<script setup lang="ts">
// One of RunView's three "linked to this session" header menus — Slack threads,
// todos or notes (the mirror of the windows' LinkedSessionsMenu): an outline
// icon button with the count badge opening the linked items; a row opens its
// window (Slack threads / Todos / Notes) on that item.
//
// slack  shown only when the session has linked threads (as before).
// todo / note  always shown (badge hidden at 0, an empty-state line instead),
//        and end with "+ New todo / note" — the capture window, filed under this
//        session's project and run, so the new item comes back as a `captured`
//        link. That replaces the header's old New todo / New note buttons.
//
// The host loads the items once for all three (useRunLinkedItems).
import { computed, type Component } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Hash, ListTodo, Plus, Slack, StickyNote } from 'lucide-vue-next'
import { Button, DropdownItem, DropdownMenu, DropdownSeparator } from '@/components/ui'
import { openSlackWindow } from '@/lib/slackWindow'
import { openItemListWindow } from '@/lib/itemListWindow'
import { firstLine } from '@/lib/itemText'
import type { RunLinkedItem } from '@/lib/sessionLinks'
import type { SlackThreadSummary } from '@/stores/slackThreads'

const props = withDefaults(
  defineProps<{
    kind: 'slack' | 'todo' | 'note'
    /** kind 'slack'. */
    threads?: SlackThreadSummary[]
    /** kind 'todo' / 'note'. */
    items?: RunLinkedItem[]
  }>(),
  { threads: () => [], items: () => [] },
)

const emit = defineEmits<{ create: [] }>()

const { t } = useI18n()

const ICONS: Record<'slack' | 'todo' | 'note', Component> = {
  slack: Slack,
  todo: ListTodo,
  note: StickyNote,
}

const count = computed(() => (props.kind === 'slack' ? props.threads.length : props.items.length))

const heading = computed(() =>
  props.kind === 'slack'
    ? t('slackThread.window.title')
    : props.kind === 'todo'
      ? t('item.navTodos')
      : t('item.navNotes'),
)

const buttonTitle = computed(() =>
  props.kind === 'slack'
    ? t('sessionLink.runSlack', count.value)
    : props.kind === 'todo'
      ? t('sessionLink.runTodos', count.value)
      : t('sessionLink.runNotes', count.value),
)

function channelOf(th: SlackThreadSummary): string {
  return th.channel ?? th.channelId ?? ''
}

/** A note's title (else its first line); a todo's first line. */
function itemLabel(it: RunLinkedItem): string {
  return it.title.trim() || firstLine(it.text) || t('item.untitled')
}

/** A note's first prose line after its title (the sidebar's preview). */
function notePreview(it: RunLinkedItem): string {
  const lines = it.text.split('\n').map(firstLine).filter(Boolean)
  return (it.title.trim() ? lines : lines.slice(1)).join(' ')
}

function open(id: string) {
  if (props.kind === 'slack') void openSlackWindow({ id })
  else void openItemListWindow(props.kind, { id })
}
</script>

<template>
  <DropdownMenu v-if="kind !== 'slack' || count" align="right">
    <template #trigger>
      <Button variant="outline" size="icon" class="relative" :title="buttonTitle" :aria-label="buttonTitle">
        <component :is="ICONS[kind]" class="h-4 w-4" :stroke-width="2" />
        <span
          v-if="count"
          class="absolute -right-1 -top-1 flex h-3.5 min-w-[14px] items-center justify-center rounded-full bg-primary px-0.5 text-[9px] font-semibold leading-none text-primary-foreground"
        >
          {{ count }}
        </span>
      </Button>
    </template>
    <div class="w-[22rem]">
      <div class="px-2.5 pb-0.5 pt-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground/70">
        {{ heading }}
      </div>
      <div class="max-h-[22rem] overflow-y-auto">
        <!-- Slack threads -->
        <template v-if="kind === 'slack'">
          <DropdownItem
            v-for="th in threads"
            :key="th.id"
            :title="t('sessionLink.openSlack')"
            @click="open(th.id)"
          >
            <Slack class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
            <span class="min-w-0 flex-1 truncate">{{ th.title || t('slackThread.untitled') }}</span>
            <span v-if="channelOf(th)" class="flex shrink-0 items-center text-[11px] text-muted-foreground">
              <Hash class="h-3 w-3" :stroke-width="2" />{{ channelOf(th) }}
            </span>
          </DropdownItem>
        </template>

        <template v-else>
          <p v-if="!items.length" class="px-2.5 py-1.5 text-[11px] text-muted-foreground" @click.stop>
            {{ kind === 'todo' ? t('sessionLink.emptyTodos') : t('sessionLink.emptyNotes') }}
          </p>
          <!-- Todos: checkbox state + first line -->
          <template v-if="kind === 'todo'">
            <DropdownItem
              v-for="td in items"
              :key="td.id"
              :title="t('sessionLink.openTodo')"
              @click="open(td.id)"
            >
              <span
                class="flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-[4px] border"
                :class="td.done ? 'border-primary bg-primary text-primary-foreground' : 'border-muted-foreground/40'"
              >
                <Check v-if="td.done" class="h-2 w-2" :stroke-width="3" />
              </span>
              <span class="min-w-0 flex-1 truncate" :class="td.done && 'text-muted-foreground line-through'">
                {{ itemLabel(td) }}
              </span>
            </DropdownItem>
          </template>
          <!-- Notes: title over a one-line preview -->
          <template v-else>
            <DropdownItem
              v-for="n in items"
              :key="n.id"
              class="items-start"
              :title="t('sessionLink.openNote')"
              @click="open(n.id)"
            >
              <StickyNote class="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
              <span class="block min-w-0 flex-1">
                <span class="block truncate">{{ itemLabel(n) }}</span>
                <span v-if="notePreview(n)" class="block truncate text-[11px] text-muted-foreground">
                  {{ notePreview(n) }}
                </span>
              </span>
            </DropdownItem>
          </template>
        </template>
      </div>

      <!-- Capture a new one, filed under this session (comes back linked). -->
      <template v-if="kind !== 'slack'">
        <DropdownSeparator />
        <DropdownItem variant="primary" @click="emit('create')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ kind === 'todo' ? t('item.newTodo') : t('item.newNote') }}
        </DropdownItem>
      </template>
    </div>
  </DropdownMenu>
</template>
