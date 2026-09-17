<script setup lang="ts">
// The rows of the file menu — ONE list, rendered by both surfaces that offer
// file actions:
//   • FileTree      — inside the right-click ContextMenu
//   • FileViewer    — inside the ⋯ DropdownMenu of the file window
//
// They used to be two separate lists that had quietly diverged: the viewer could
// copy content and open in the default app but not rename, delete, duplicate or
// paste; the tree could do all of those but not the first two. Same object, two
// different sets of powers depending on where you clicked it.
//
// Creating (new file / new folder) is the one difference left, and it is a real
// one: it needs a target FOLDER, which only the tree has. Hosts opt into it with
// `show-create` and handle the two events.
//
// The actions themselves live in lib/fileActions — including the cross-webview
// bridges for "mention in chat" and image compare.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AtSign, Chrome, ClipboardPaste, Code, Columns2, Copy, CopyPlus, ExternalLink,
  FilePlus, FolderOpen, FolderPlus, Link, Pencil, Scissors, Trash2,
} from 'lucide-vue-next'
import { DropdownItem, DropdownSeparator } from '@/components/ui'
import { useFileActions, type FileTarget } from '@/lib/fileActions'
import { isImagePath } from '@/stores/imageCompare'

const props = withDefaults(
  defineProps<{
    projectPath: string
    /** What the menu acts on; null means "the project root" (empty-space click). */
    target: FileTarget | null
    /** Offer New file / New folder — only where a target folder is known. */
    showCreate?: boolean
  }>(),
  { showCreate: false },
)

const emit = defineEmits<{
  createFile: []
  createFolder: []
  /** Hosts rename their own way: inline in the tree, a prompt in the viewer. */
  rename: [target: FileTarget]
  /** The target is gone; the viewer closes its window, the tree just refreshes. */
  deleted: [target: FileTarget]
}>()

const { t } = useI18n()
const actions = useFileActions()

const isImage = computed(() => !!props.target && !props.target.isDir && isImagePath(props.target.path))
const isFile = computed(() => !!props.target && !props.target.isDir)

async function onDelete() {
  const target = props.target
  if (!target) return
  if (await actions.remove(props.projectPath, target)) emit('deleted', target)
}
</script>

<template>
  <!-- Create (tree only: it is the surface that knows which folder to create in) -->
  <template v-if="showCreate">
    <DropdownItem @click="emit('createFile')">
      <FilePlus class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.newFile') }}
    </DropdownItem>
    <DropdownItem @click="emit('createFolder')">
      <FolderPlus class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.newFolder') }}
    </DropdownItem>
    <DropdownSeparator />
  </template>

  <!-- Clipboard -->
  <template v-if="target">
    <DropdownItem @click="actions.copy(target)">
      <Copy class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.copy') }}
    </DropdownItem>
    <DropdownItem @click="actions.cut(target)">
      <Scissors class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.cut') }}
    </DropdownItem>
    <DropdownItem @click="actions.duplicate(projectPath, target)">
      <CopyPlus class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.duplicate') }}
    </DropdownItem>
  </template>
  <DropdownItem v-if="actions.clipboard.value" @click="actions.paste(projectPath, target)">
    <ClipboardPaste class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
    {{ t('files.tree.paste') }}
  </DropdownItem>

  <!-- Edit -->
  <template v-if="target">
    <DropdownSeparator />
    <DropdownItem @click="emit('rename', target)">
      <Pencil class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.rename') }}
    </DropdownItem>
    <DropdownItem variant="destructive" @click="onDelete">
      <Trash2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('common.delete') }}
    </DropdownItem>

    <!-- Copy out -->
    <DropdownSeparator />
    <DropdownItem v-if="isFile" @click="actions.copyContent(projectPath, target)">
      <Copy class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.copyContent') }}
    </DropdownItem>
    <DropdownItem @click="actions.copyAbsPath(projectPath, target)">
      <Copy class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.copyPath') }}
    </DropdownItem>
    <DropdownItem @click="actions.copyRelPath(target)">
      <Link class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.copyRelativePath') }}
    </DropdownItem>

    <!-- Open elsewhere -->
    <DropdownSeparator />
    <DropdownItem @click="actions.revealInFolder(projectPath, target)">
      <FolderOpen class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.revealInFinder') }}
    </DropdownItem>
    <DropdownItem @click="actions.openInDefaultApp(projectPath, target)">
      <ExternalLink class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.openInDefaultApp') }}
    </DropdownItem>
    <DropdownItem @click="actions.openInVscode(projectPath, target)">
      <Code class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.openInVscode') }}
    </DropdownItem>
    <DropdownItem @click="actions.openInChrome(projectPath, target)">
      <Chrome class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.openInChrome') }}
    </DropdownItem>
    <DropdownItem @click="actions.mentionInChat(projectPath, target)">
      <AtSign class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
      {{ t('files.tree.mentionInChat') }}
    </DropdownItem>

    <!-- Compare (images only). The main window decides whether this starts a
         comparison or completes the one already waiting. -->
    <template v-if="isImage">
      <DropdownSeparator />
      <DropdownItem @click="actions.compareImage(projectPath, target)">
        <Columns2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
        {{ t('files.tree.compareWith') }}
      </DropdownItem>
    </template>
  </template>
</template>
