<script setup lang="ts">
// App-wide host for the image comparison feature (mounted once in App.vue's main
// window). Renders, off the imageCompare store:
//   • a floating banner while a first image is picked, awaiting the second;
//   • the project image picker for choosing the second image;
//   • the full-screen side-by-side compare view.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Images, X, Search } from 'lucide-vue-next'
import { Modal } from '@/components/ui'
import ImageCompareView from '@/components/ImageCompareView.vue'
import { useImageCompareStore } from '@/stores/imageCompare'

const { t } = useI18n()
const store = useImageCompareStore()

const query = ref('')
watch(
  () => store.pickerOpen,
  (open) => { if (open) query.value = '' },
)

function basename(p: string): string {
  return p.split('/').pop() ?? p
}

// Picker excludes the already-chosen first image and honours the search box.
const filteredImages = computed(() => {
  const q = query.value.trim().toLowerCase()
  return store.images.filter((p) => {
    if (p === store.pendingFirst) return false
    return !q || p.toLowerCase().includes(q)
  })
})

const bannerVisible = computed(
  () => store.hasPending && !store.open && !store.pickerOpen,
)
</script>

<template>
  <!-- Pending banner: prompts for the second image after the first is chosen -->
  <Transition
    enter-active-class="transition duration-200 ease-out"
    enter-from-class="opacity-0 translate-y-2"
    leave-active-class="transition duration-150 ease-in"
    leave-to-class="opacity-0 translate-y-2"
  >
    <div
      v-if="bannerVisible"
      class="fixed bottom-5 left-1/2 -translate-x-1/2 z-[60] flex items-center gap-3 rounded-lg border border-border bg-popover px-4 py-2.5 shadow-xl shadow-black/30"
    >
      <Images class="h-4 w-4 text-primary shrink-0" :stroke-width="1.75" />
      <span class="text-xs text-foreground/80">
        {{ t('files.compare.pendingBanner', { name: store.pendingName }) }}
      </span>
      <button
        class="flex items-center gap-1.5 h-7 px-2.5 rounded-md text-[11px] font-medium bg-primary/15 text-primary hover:bg-primary/25 transition-colors cursor-pointer shrink-0"
        @click="store.openPicker()"
      >
        <Search class="h-3.5 w-3.5" :stroke-width="1.75" /> {{ t('files.compare.chooseFromList') }}
      </button>
      <button
        class="flex items-center justify-center h-7 w-7 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
        :title="t('common.cancel')"
        @click="store.cancelPending()"
      >
        <X class="h-4 w-4" :stroke-width="1.75" />
      </button>
    </div>
  </Transition>

  <!-- Second-image picker -->
  <Modal
    :open="store.pickerOpen"
    :title="t('files.compare.pickSecond')"
    size="lg"
    scroll-body
    @close="store.cancelPending()"
  >
    <div class="flex flex-col min-h-0">
      <!-- Search -->
      <div class="flex items-center gap-2 border-b border-border px-4 py-2.5 shrink-0">
        <Search class="h-3.5 w-3.5 text-foreground/40 shrink-0" :stroke-width="1.75" />
        <input
          v-model="query"
          type="text"
          :placeholder="t('files.compare.searchPlaceholder')"
          class="flex-1 bg-transparent text-sm text-foreground placeholder:text-foreground/40 outline-none"
        />
      </div>
      <!-- Image list -->
      <div class="max-h-[55vh] overflow-auto p-1.5">
        <div v-if="store.loadingImages" class="p-6 text-center text-xs text-muted-foreground">
          {{ t('common.loading') }}
        </div>
        <div v-else-if="filteredImages.length === 0" class="p-6 text-center text-xs text-muted-foreground">
          {{ t('files.compare.noImages') }}
        </div>
        <button
          v-for="img in filteredImages"
          :key="img"
          class="w-full flex items-center gap-2 rounded-md px-2.5 py-1.5 text-left hover:bg-accent transition-colors cursor-pointer"
          @click="store.selectSecond(img)"
        >
          <Images class="h-3.5 w-3.5 text-foreground/40 shrink-0" :stroke-width="1.75" />
          <span class="flex-1 min-w-0">
            <span class="block text-[13px] text-foreground truncate">{{ basename(img) }}</span>
            <span class="block text-[10px] font-mono text-foreground/40 truncate">{{ img }}</span>
          </span>
        </button>
      </div>
    </div>
  </Modal>

  <!-- Compare view (full-screen) -->
  <Modal
    :open="store.open"
    size="full"
    scroll-body
    hide-header
    @close="store.close()"
  >
    <ImageCompareView
      v-if="store.open"
      :project-path="store.projectPath"
      :left="store.left"
      :right="store.right"
      @close="store.close()"
      @swap="store.swap()"
    />
  </Modal>
</template>
