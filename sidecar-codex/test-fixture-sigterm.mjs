#!/usr/bin/env node
// Fixture for test-exit-signal.mjs: an "app-server" that gets killed by SIGTERM
// shortly after start (like a crash / OOM-kill), without answering anything.
process.stdin.resume()
setTimeout(() => process.kill(process.pid, 'SIGTERM'), 300)
