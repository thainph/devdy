// Module resolve hook: `@anthropic-ai/claude-agent-sdk` → the local mock.
export async function resolve(specifier, context, nextResolve) {
  if (specifier === '@anthropic-ai/claude-agent-sdk') {
    return { url: new URL('./mock-sdk.mjs', import.meta.url).href, shortCircuit: true }
  }
  return nextResolve(specifier, context)
}
