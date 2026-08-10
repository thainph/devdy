// Project-context helpers for the current run's working directory
// (DEVDY_PROJECT_PATH): a bounded file tree and read-only git introspection.
// Git calls shell out to the `git` binary with a hard timeout and never mutate.

import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const SKIP_DIRS = new Set([
  'node_modules', '.git', 'target', 'dist', 'build', '.next', '.nuxt',
  '.svelte-kit', 'vendor', '.venv', 'venv', '__pycache__', '.cache',
  '.devdy', '.idea', '.vscode', 'coverage', '.turbo', 'out',
]);

function projectRoot() {
  const p = process.env.DEVDY_PROJECT_PATH;
  if (!p) throw new Error('DEVDY_PROJECT_PATH is not set — no current project');
  return p;
}

/**
 * Render a bounded directory tree rooted at the project (or a subdir).
 * Caps total entries and depth so huge repos don't blow up the context.
 */
export function fileTree({ subpath = '', maxDepth = 3, maxEntries = 400 } = {}) {
  const root = projectRoot();
  const start = path.resolve(root, subpath || '.');
  if (!start.startsWith(path.resolve(root))) throw new Error('subpath escapes the project directory');
  if (!fs.existsSync(start)) throw new Error(`path not found: ${subpath || '.'}`);

  const lines = [];
  let count = 0;
  let truncated = false;

  const walk = (dir, depth, prefix) => {
    if (depth > maxDepth || truncated) return;
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    entries = entries
      .filter((e) => !e.name.startsWith('.') || e.name === '.env.example')
      .filter((e) => !(e.isDirectory() && SKIP_DIRS.has(e.name)))
      .sort((a, b) => (a.isDirectory() === b.isDirectory() ? a.name.localeCompare(b.name) : a.isDirectory() ? -1 : 1));
    for (const e of entries) {
      if (count >= maxEntries) {
        truncated = true;
        return;
      }
      count += 1;
      lines.push(`${prefix}${e.isDirectory() ? '📁 ' : ''}${e.name}`);
      if (e.isDirectory()) walk(path.join(dir, e.name), depth + 1, `${prefix}  `);
    }
  };

  walk(start, 1, '');
  const header = path.relative(root, start) || path.basename(root);
  let out = `${header}/\n${lines.join('\n')}`;
  if (truncated) out += `\n… [truncated at ${maxEntries} entries — narrow with subpath or lower maxDepth]`;
  return out;
}

function git(args) {
  try {
    return execFileSync('git', args, {
      cwd: projectRoot(),
      encoding: 'utf8',
      timeout: 10000,
      maxBuffer: 4 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'pipe'],
    }).trim();
  } catch (e) {
    const msg = (e && (e.stderr || e.message)) || String(e);
    throw new Error(`git ${args[0]} failed: ${String(msg).split('\n')[0]}`);
  }
}

export function gitStatus() {
  const branch = git(['rev-parse', '--abbrev-ref', 'HEAD']);
  const status = git(['status', '--short', '--branch']);
  const lastCommits = git(['log', '-5', '--oneline', '--no-decorate']);
  return `Branch: ${branch}\n\n## git status\n${status || '(clean)'}\n\n## recent commits\n${lastCommits || '(none)'}`;
}

export function gitDiff({ staged = false, stat = false, maxChars = 12000 } = {}) {
  const args = ['diff'];
  if (staged) args.push('--cached');
  if (stat) args.push('--stat');
  let out = git(args);
  if (!out) return staged ? '(no staged changes)' : '(no unstaged changes)';
  if (out.length > maxChars) out = `${out.slice(0, maxChars)}\n… [diff truncated, ${out.length - maxChars} more chars — use stat=true for an overview]`;
  return out;
}
