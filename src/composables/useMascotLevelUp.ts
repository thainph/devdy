import { ref } from 'vue'

// Shared one-shot signal for the breakthrough (level-up) VFX. CyberFoxHost bumps
// it when a genuine in-session level-up happens; the floating mascot (and the
// desktop pet) watch the timestamp and replay the rising-rings effect.
const levelUpAt = ref(0)

export function triggerMascotLevelUp() {
  levelUpAt.value = Date.now()
}

export function useMascotLevelUp() {
  return { levelUpAt }
}
