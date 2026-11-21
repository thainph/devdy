// Icon + label of any tab of the strip (project, surface, page), shared by the
// tab strip and the command palette.
import type { Component } from 'vue'
import { FolderOpen, PanelTop } from 'lucide-vue-next'
import type { TabEntry } from '@/stores/workspaceTabs'
import { SURFACE_META } from '@/lib/surfaceMeta'
import { pageRouteFor } from '@/lib/navigation'

type Translate = (key: string) => string

export function entryIcon(e: TabEntry): Component {
  if (e.type === 'project') return FolderOpen
  if (e.type === 'surface') return SURFACE_META[e.surface].icon
  return pageRouteFor(e.path)?.icon ?? PanelTop
}

export function entryLabel(e: TabEntry, t: Translate, projectName: (id: string) => string): string {
  if (e.type === 'project') return projectName(e.projectId)
  if (e.type === 'surface') {
    if (e.surface === 'file' && e.params.path) return e.params.path.split('/').pop() || e.params.path
    return t(SURFACE_META[e.surface].labelKey)
  }
  const page = pageRouteFor(e.path)
  return page ? t(page.labelKey) : e.path
}
