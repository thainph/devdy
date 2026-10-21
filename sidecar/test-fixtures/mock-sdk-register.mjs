// Preloaded with `node --import` by the mocked sidecar tests: swaps the real
// Agent SDK for `mock-sdk.mjs` so the sidecar runs without a CLI or network.
import { register } from 'node:module'

register('./mock-sdk-hooks.mjs', import.meta.url)
