// Minimal stand-in for `query()` of @anthropic-ai/claude-agent-sdk, driven by
// MOCK_SCENARIO. Like the real SDK, it keeps the turn running after the input
// stream ends, so a tool can still ask for permission once stdin is closed.
//
//   permission  — one turn whose tool asks permission; reports the decision.
//   abort       — the CLI withdraws the request (signal aborts) after 200ms.
//   after-close — waits for the input to end, THEN a tool asks permission.
//   throw       — the session loop fails (like a CLI crash).

export function query({ prompt, options }) {
  const scenario = process.env.MOCK_SCENARIO || 'permission'
  const input = prompt[Symbol.asyncIterator]()

  async function ask(toolName) {
    const ac = new AbortController()
    if (scenario === 'abort') setTimeout(() => ac.abort(), 200)
    const res = await options.canUseTool(toolName, { file_path: 'out.txt' }, { signal: ac.signal, toolUseID: 'toolu_mock' })
    return { type: 'result', subtype: 'success', mock_tool: toolName, mock_behavior: res.behavior, mock_message: res.message ?? null }
  }

  async function* run() {
    await input.next() // the first user prompt
    if (scenario === 'throw') throw new Error('mock CLI crashed')
    // Echo the (JSON-safe) options so tests can assert what reached the SDK.
    yield { type: 'system', subtype: 'init', session_id: 'mock-session', model: 'mock', mock_options: JSON.parse(JSON.stringify(options)), mock_has_can_use_tool: typeof options.canUseTool === 'function' }
    if (scenario === 'after-close') {
      while (!(await input.next()).done) { /* drain follow-ups until stdin closes */ }
      yield await ask('Edit')
      return
    }
    yield await ask('Write')
    // Keep serving turns until the input stream ends.
    while (!(await input.next()).done) {
      yield { type: 'result', subtype: 'success' }
    }
  }

  return run()
}
