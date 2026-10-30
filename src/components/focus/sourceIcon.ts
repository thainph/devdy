import type { Component } from 'vue'
import { Github, Gitlab, Link2, Slack, SquareTerminal } from 'lucide-vue-next'
import type { TodoSourceType } from '@/lib/focus'

/** Icon of a todo's source (row button, drawer, capture chip). */
export const SOURCE_ICON: Record<TodoSourceType, Component> = {
  slack: Slack,
  github: Github,
  gitlab: Gitlab,
  run: SquareTerminal,
  url: Link2,
}
