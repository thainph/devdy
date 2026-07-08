import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'
import type { ApplyAllOutcome } from '@/stores/rules'

export type { ApplyAllOutcome } from '@/stores/rules'

export type SkillTarget = 'claude' | 'codex' | 'both'

export interface Skill {
  id: string
  name: string
  description: string
  target: SkillTarget
  source_path: string
  updated_at: string
}

export interface SkillContent {
  skill: Skill
  content: string
}

export const useSkillsStore = defineStore('skills', () => {
  const skills = ref<Skill[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetchSkills() {
    loading.value = true
    error.value = null
    try {
      skills.value = await invoke<Skill[]>('list_skills')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  async function getSkill(id: string): Promise<SkillContent> {
    return invoke<SkillContent>('get_skill', { id })
  }

  async function createSkill(payload: { name: string; description: string; target: SkillTarget; content: string }): Promise<Skill> {
    const skill = await invoke<Skill>('create_skill', { payload })
    await fetchSkills()
    return skill
  }

  async function updateSkill(payload: { id: string; name: string; description: string; target: SkillTarget; content: string }): Promise<Skill> {
    const skill = await invoke<Skill>('update_skill', { payload })
    await fetchSkills()
    return skill
  }

  async function deleteSkill(id: string): Promise<void> {
    await invoke('delete_skill', { id })
    await fetchSkills()
  }

  async function importSkillZip(zipPath: string): Promise<Skill> {
    const skill = await invoke<Skill>('import_skill_zip', { zipPath })
    await fetchSkills()
    return skill
  }

  async function exportSkillZip(id: string, destPath: string): Promise<void> {
    await invoke('export_skill_zip', { id, destPath })
  }

  async function applySkillToAllProjects(id: string): Promise<ApplyAllOutcome> {
    return invoke<ApplyAllOutcome>('apply_skill_to_all_projects', { skillId: id })
  }

  return { skills, loading, error, fetchSkills, getSkill, createSkill, updateSkill, deleteSkill, importSkillZip, exportSkillZip, applySkillToAllProjects }
})
