#!/usr/bin/env node
// Deterministic JSON-RPC fixture for test-deny.mjs.
import { writeFileSync } from 'node:fs'

const marker = process.env.DEVDY_DENY_TEST_MARKER
const mcpMode = process.env.DEVDY_MCP_PERMISSION_KEY_TEST === '1'
let buffer = ''

function send(message) {
  process.stdout.write(JSON.stringify(message) + '\n')
}

process.stdin.on('data', (chunk) => {
  buffer += chunk.toString()
  let newline
  while ((newline = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, newline).trim()
    buffer = buffer.slice(newline + 1)
    if (!line) continue
    let message
    try { message = JSON.parse(line) } catch { continue }

    if (message.method === 'initialize') {
      send({ jsonrpc: '2.0', id: message.id, result: {} })
    } else if (message.method === 'thread/start') {
      send({
        jsonrpc: '2.0',
        id: message.id,
        result: { thread: { id: 'fixture-thread' }, model: 'fixture' },
      })
    } else if (message.method === 'account/rateLimits/read') {
      send({ jsonrpc: '2.0', id: message.id, result: {} })
    } else if (message.method === 'turn/start') {
      send({ jsonrpc: '2.0', id: message.id, result: { turn: { id: 'fixture-turn' } } })
      if (mcpMode) {
        send({
          jsonrpc: '2.0',
          id: 92,
          method: 'mcpServer/elicitation/request',
          params: {
            serverName: 'Google Drive',
            toolName: 'list_files',
            mode: 'permission',
            message: 'Allow listing Drive files?',
          },
        })
        continue
      }
      send({
        jsonrpc: '2.0',
        id: 91,
        method: 'item/commandExecution/requestApproval',
        params: { command: `touch ${marker}`, reason: 'fixture approval' },
      })
    } else if (message.id === 91 && message.result) {
      const decision = message.result.decision
      const denied = decision === 'cancel' || decision === 'decline'
      if (!denied && marker) writeFileSync(marker, 'executed')
      send({
        jsonrpc: '2.0',
        method: 'item/completed',
        params: {
          item: {
            id: 'fixture-command',
            type: 'commandExecution',
            command: `touch ${marker}`,
            status: denied ? 'declined' : 'completed',
            exitCode: denied ? null : 0,
            aggregatedOutput: '',
          },
        },
      })
      send({
        jsonrpc: '2.0',
        method: 'turn/completed',
        params: { turn: { id: 'fixture-turn', status: 'completed' } },
      })
    } else if (message.id === 92 && message.result) {
      send({
        jsonrpc: '2.0',
        method: 'turn/completed',
        params: { turn: { id: 'fixture-turn', status: 'completed' } },
      })
    }
  }
})
