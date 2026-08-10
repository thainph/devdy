// Non-interactive SSH exec against a Devdy-managed server. Mirrors Devdy's own
// connection probe (BatchMode + accept-new + identity file for key auth), so it
// works for ssh-agent or passphrase-less keys and never prompts. Passphrase-
// protected keys must be loaded into ssh-agent (Devdy keeps passphrases in the
// Keychain, out of reach of this separate process — by design).

import { execFileSync } from 'node:child_process';

export function runOnServer(server, command, { timeoutMs = 30000 } = {}) {
  const cmd = String(command || '').trim();
  if (!cmd) throw new Error('command is required');

  const args = [
    '-o', 'BatchMode=yes',
    '-o', 'ConnectTimeout=10',
    '-o', 'StrictHostKeyChecking=accept-new',
    '-p', String(server.port || 22),
  ];
  if (server.auth_method === 'key' && server.private_key_path) {
    args.push('-i', server.private_key_path);
  }
  args.push(`${server.username}@${server.host}`, cmd);

  try {
    const out = execFileSync('ssh', args, {
      encoding: 'utf8',
      timeout: timeoutMs,
      maxBuffer: 4 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    return { ok: true, output: out.trim() || '(no output)' };
  } catch (e) {
    if (e && e.code === 'ETIMEDOUT') {
      return { ok: false, output: `Timed out after ${timeoutMs / 1000}s` };
    }
    // execFileSync attaches stdout/stderr of the failed command.
    const stdout = e && e.stdout ? String(e.stdout) : '';
    const stderr = e && e.stderr ? String(e.stderr) : '';
    const tail = `${stdout}${stderr}`.trim() || (e && e.message) || 'ssh failed';
    return { ok: false, output: tail };
  }
}
