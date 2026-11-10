import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'

/** A GitHub Project V2 board configured under a GitHub account, enabled per project. */
export interface GithubBoard {
  id: string
  account_id: string
  url: string
  title: string
  /** JSON { boardId, ownerType, owner, number, startFieldId, deadlineFieldId, statusFieldId }. */
  field_mappings: string
  created_at: string
}

export interface BoardField {
  id: string
  name: string
  dataType: string
}

export interface BoardInfo {
  id: string
  title: string
  url: string
  number: number
  owner: string
  ownerType: string
  fields: BoardField[]
}

export interface BoardFieldMapping {
  startFieldId: string | null
  deadlineFieldId: string | null
  statusFieldId: string | null
}

export function parseBoardMapping(json: string | null | undefined): BoardFieldMapping {
  try {
    const m = JSON.parse(json ?? 'null') ?? {}
    return {
      startFieldId: m.startFieldId ?? null,
      deadlineFieldId: m.deadlineFieldId ?? null,
      statusFieldId: m.statusFieldId ?? null,
    }
  } catch {
    return { startFieldId: null, deadlineFieldId: null, statusFieldId: null }
  }
}

export const useGithubBoardsStore = defineStore('githubBoards', () => {
  const boards = ref<GithubBoard[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  async function fetch() {
    loading.value = true
    error.value = null
    try {
      boards.value = await invoke<GithubBoard[]>('list_github_boards')
    } catch (e) {
      error.value = String(e)
    } finally {
      loading.value = false
    }
  }

  /** Read a board's fields with the account's PAT, for the field mapper. */
  async function resolve(accountId: string, url: string): Promise<BoardInfo> {
    return invoke<BoardInfo>('resolve_github_board', { accountId, url })
  }

  /** Save a board under an account; an existing board of the same `url` is updated. */
  async function save(
    accountId: string,
    url: string,
    info: BoardInfo,
    mapping: BoardFieldMapping,
  ): Promise<GithubBoard> {
    const fieldMappings = JSON.stringify({
      boardId: info.id,
      ownerType: info.ownerType,
      owner: info.owner,
      number: info.number,
      ...mapping,
    })
    const board = await invoke<GithubBoard>('save_github_board', {
      accountId,
      url,
      title: info.title,
      fieldMappings,
    })
    await fetch()
    return board
  }

  async function remove(id: string): Promise<void> {
    await invoke('delete_github_board', { id })
    await fetch()
  }

  return { boards, loading, error, fetch, resolve, save, remove }
})
