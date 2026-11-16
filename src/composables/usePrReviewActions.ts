import { useI18n } from 'vue-i18n'
import { usePrInboxStore, prKey, type PrInboxItem } from '@/stores/prInbox'
import { useRunsStore } from '@/stores/runs'
import { useConductorStore } from '@/stores/conductor'
import { useAppSettingsStore } from '@/stores/appSettings'
import { resolvePermissionMode } from '@/lib/engineOptions'
import { canReReview } from '@/lib/prInboxFilters'
import { useToast } from '@/composables/useToast'

/**
 * The PR Inbox review actions, shared by the PR Inbox window and the Work
 * Dashboard: start an AI review session for a PR, open the one already linked,
 * or review again in that session after new pushes.
 */
export function usePrReviewActions(opts: {
  /** Show the review session already linked to the PR. */
  openRun: (item: PrInboxItem) => void
  /** Effective status of the PR's linked run (live status over the DB snapshot). */
  runStatus: (item: PrInboxItem) => string | null
}) {
  const { t } = useI18n()
  const store = usePrInboxStore()
  const runsStore = useRunsStore()
  const conductorStore = useConductorStore()
  const appSettings = useAppSettingsStore()
  const { toast } = useToast()

  // Create + launch a review run for a mapped PR with no linked run. Throws on
  // failure; returns false when there was nothing to start (unmapped, already
  // linked, or already being started).
  async function startReview(item: PrInboxItem): Promise<boolean> {
    if (!item.mapped || !item.project_id || !item.repo_id || item.existing_run_id) return false
    const key = prKey(item)
    if (store.reviewingKeys.has(key)) return false
    store.setReviewing(key, true)
    try {
      // Allow reviewing a PR even when no linked issue can be resolved — the PR
      // inbox should run the review regardless of issue linkage.
      // Title the session with the PR's author and original title so inbox reviews
      // are told apart at a glance.
      const title = [`PR #${item.number}`, item.author_login && `@${item.author_login}`, item.title?.trim()]
        .filter(Boolean)
        .join(' · ')
      const run = await runsStore.fetchPr(
        item.project_id,
        item.repo_id,
        item.number,
        undefined,
        true,
        title,
        // Review as the account that was asked to review, not the project default.
        item.account_id,
      )
      // Every session is a conductor: launch it like RunView's Issue/PR Run button
      // — empty goal (default review prompt, labelled "PR #N"), default worker cap,
      // and the effective permission mode (this screen has no per-run pick).
      await conductorStore.start(run.id, '', {
        permissionModeOverride: resolvePermissionMode(null, appSettings.settings?.default_permission_mode),
      })
      // Link immediately so a second click resumes instead of duplicating. The run
      // streams in the background (liveRuns / ActiveRunsDock) — stay on this screen.
      store.linkRun(key, run.id, 'running')
      return true
    } finally {
      store.setReviewing(key, false)
    }
  }

  // Start a review, or resume the linked one. A PR that already has a review run
  // (in DB or just created) never spawns a second session — we navigate to it.
  async function handleReview(item: PrInboxItem) {
    if (item.existing_run_id) { opts.openRun(item); return }
    try {
      if (await startReview(item)) {
        // Stay on this screen (no focus jump — the window is for multitasking):
        // the button turns into "Reviewing" / "View review", which opens it.
        toast.success(t('prInbox.reviewing', { repo: item.repo, number: item.number }))
      }
    } catch (e) {
      toast.error(String(e))
    }
  }

  // Review a PR again IN its existing session — offered once the linked review
  // has finished, highlighted when new commits landed after it. Re-fetches the
  // run's task file (latest diff, comments, reviews; also moves its head_sha so
  // the "updated" flag clears), resumes the session and sends the configured
  // re-review prompt — the same resume → send path as Remote Control. The
  // backend's `run:activated` makes the main window's liveRuns / dock (and this
  // window's status feed) pick the turn up.
  async function handleReReview(item: PrInboxItem) {
    const key = prKey(item)
    const runId = item.existing_run_id
    if (!runId || !canReReview(item, opts.runStatus(item), store.reviewingKeys.has(key))) return
    store.setReviewing(key, true)
    try {
      const prompt = await store.loadReReviewPrompt()
      await runsStore.refetchRun(runId)
      await runsStore.resumeRun(runId, resolvePermissionMode(null, appSettings.settings?.default_permission_mode))
      await runsStore.sendUserMessage(runId, prompt)
      store.linkRun(key, runId, 'running')
      toast.success(t('prInbox.reviewing', { repo: item.repo, number: item.number }))
    } catch (e) {
      toast.error(t('prInbox.reReviewFailed', { error: String(e) }))
    } finally {
      store.setReviewing(key, false)
    }
  }

  return { startReview, handleReview, handleReReview }
}
