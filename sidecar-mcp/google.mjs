#!/usr/bin/env node
// Devdy built-in MCP server: Google (stdio, JSON-RPC 2.0).
//
// One server covering Drive, Gmail and Calendar. All three share the same gate
// (an OAuth client + at least one connected account), the same credentials and
// the same lifetime, so splitting them into three processes bought nothing —
// see `with_builtin_google` in src-tauri/src/commands/mcp.rs.
//
// Injected under the key `google`, so tools are namespaced `mcp__google__*`.
// Each group keeps its own prefix because the groups genuinely collide: Drive
// and Gmail both define `search` and `delete`, and "read" means something very
// different to each. The prefix is what keeps `drive_delete` (permanent file
// removal) from being confused with `mail_delete` (permanent message removal).
//
// Zero runtime dependencies (Node 18+ global fetch).

import { runServer } from './lib/google.mjs';
import { tools as calendarTools } from './lib/google/calendar.mjs';
import { tools as driveTools } from './lib/google/drive.mjs';
import { tools as mailTools } from './lib/google/mail.mjs';

/** Copy a tool group under `<prefix><name>`. `runServer` mutates each
 *  `inputSchema.properties` to add the shared `account` argument, so clone both
 *  levels and leave the group modules' own objects alone. */
function prefixed(prefix, group) {
  const out = {};
  for (const [name, tool] of Object.entries(group)) {
    const schema = tool.inputSchema || { type: 'object' };
    out[`${prefix}${name}`] = {
      ...tool,
      inputSchema: { ...schema, properties: { ...schema.properties } },
    };
  }
  return out;
}

const tools = {
  ...prefixed('drive_', driveTools),
  ...prefixed('mail_', mailTools),
  ...prefixed('cal_', calendarTools),
};

runServer({ serverInfo: { name: 'google', version: '1.0.0' }, tools, multiAccount: true });
