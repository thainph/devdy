/**
 * DY Cyber Fox leveling model.
 *
 * The mascot advances through cultivation realms driven by the user's CUMULATIVE
 * token usage over all time (SUM of run_usage.total_tokens, exposed by the
 * `get_usage_stats` command). Cumulative tokens only grow, so the derived level is
 * monotonic — the fox never regresses.
 *
 * Leveling is UNBOUNDED. The 7 base realms × 5 tiers form one "cycle" of 35
 * levels. When the fox clears a full cycle it "reincarnates" (chuyển sinh): the 7
 * realm visuals repeat from the start, but an ASCENSION counter (rendered as stars
 * ⭐) increments — so there is always a next goal and the level never caps out.
 *
 * Per-level token cost grows geometrically, so early breakthroughs feel quick and
 * later realms take real, sustained usage. Everything here is pure/deterministic so
 * it can be shared by the live mascot, the desktop-pet window and the Settings preview.
 */

export const MASCOT_REALM_IDS = [
  'luyen_khi',
  'truc_co',
  'kim_dan',
  'nguyen_anh',
  'hoa_than',
  'anh_bien',
  'van_dinh',
] as const

export type MascotRealmId = (typeof MASCOT_REALM_IDS)[number]

export const TIERS_PER_REALM = 5
/** One full pass through every base realm = 35 levels; clearing it earns a star. */
export const LEVELS_PER_CYCLE = MASCOT_REALM_IDS.length * TIERS_PER_REALM // 35

/** Tokens required to break through the FIRST level (Lv1 → Lv2). */
const FIRST_LEVEL_TOKENS = 25_000
/** Geometric growth of the per-level token cost. */
const LEVEL_GROWTH = 1.2
/** Guard against floating-point drift right on a threshold boundary. */
const LEVEL_EPSILON = 1e-9

/**
 * Cumulative tokens needed to REACH `level` (1-based). Level 1 = 0 tokens.
 * cost(k) = FIRST_LEVEL_TOKENS · GROWTH^(k-1); threshold(L) = Σ cost(1..L-1).
 * Unbounded: valid for any level ≥ 1.
 */
export function mascotLevelThreshold(level: number): number {
  const l = Math.max(1, Math.floor(level))
  if (l <= 1) return 0
  const n = l - 1
  const sum = (FIRST_LEVEL_TOKENS * (Math.pow(LEVEL_GROWTH, n) - 1)) / (LEVEL_GROWTH - 1)
  return Math.round(sum)
}

export interface MascotLevelInfo {
  /** Absolute level, 1..∞ */
  level: number
  /** Visual realm theme for the current level (cycles every LEVELS_PER_CYCLE). */
  realmId: MascotRealmId
  /** Visual realm index within the current cycle, 0..6. */
  realmIndex: number
  /** 1..5 */
  tier: number
  /** Completed full cycles = number of stars ⭐ to display (0 on the first cycle). */
  ascension: number
  /** Alias of `ascension` — the star count shown on the mascot. */
  stars: number
  totalTokens: number
  /** Cumulative token threshold already reached for the current level. */
  levelFloor: number
  /** Cumulative tokens needed for the next level (never null — leveling is infinite). */
  nextLevelAt: number
  /** 0..100 progress from the current level toward the next. */
  progress: number
  /** Kept for backward compatibility; leveling is unbounded so this is always false. */
  isMax: boolean
}

/**
 * Derive the mascot's realm / tier / level from cumulative token usage.
 *
 * Level is computed directly by inverting the geometric threshold sum (O(1), no
 * loop, no cap): tokens ≥ threshold(L) ⇔ L ≤ 1 + log_g(1 + tokens·(g−1)/A).
 */
export function mascotLevelFromTokens(totalTokens: number): MascotLevelInfo {
  const tokens = Math.max(0, Math.floor(totalTokens || 0))

  const ratio = 1 + (tokens * (LEVEL_GROWTH - 1)) / FIRST_LEVEL_TOKENS
  let level = Math.max(
    1,
    Math.floor(Math.log(ratio) / Math.log(LEVEL_GROWTH) + LEVEL_EPSILON) + 1,
  )
  // The analytic inverse can be off by 1 against the ROUNDED threshold sum right on
  // a boundary. Snap to the exact rounded-threshold definition (a couple of steps).
  while (tokens >= mascotLevelThreshold(level + 1)) level++
  while (level > 1 && tokens < mascotLevelThreshold(level)) level--

  const absoluteRealm = Math.floor((level - 1) / TIERS_PER_REALM)
  const realmIndex = absoluteRealm % MASCOT_REALM_IDS.length
  const ascension = Math.floor(absoluteRealm / MASCOT_REALM_IDS.length)
  const tier = ((level - 1) % TIERS_PER_REALM) + 1

  const levelFloor = mascotLevelThreshold(level)
  const nextLevelAt = mascotLevelThreshold(level + 1)
  const span = nextLevelAt - levelFloor
  const progress =
    span <= 0 ? 0 : Math.max(0, Math.min(100, Math.round(((tokens - levelFloor) / span) * 100)))

  return {
    level,
    realmId: MASCOT_REALM_IDS[realmIndex],
    realmIndex,
    tier,
    ascension,
    stars: ascension,
    totalTokens: tokens,
    levelFloor,
    nextLevelAt,
    progress,
    isMax: false,
  }
}
