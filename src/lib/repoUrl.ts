// Parse a pasted repository URL (or "owner/repo" shorthand) into the fields the
// project repo form needs, so users can add a repo by pasting a link instead of
// filling provider / owner / repo by hand.
//
// Supported shapes:
//   https://github.com/owner/repo            → github
//   https://github.com/owner/repo.git        → github
//   git@github.com:owner/repo.git            → github
//   github.com/owner/repo                    → github
//   owner/repo                               → github (shorthand)
//   https://gitlab.com/group/sub/repo        → gitlab (nested groups kept in path)
//   git@gitlab.example.com:group/repo.git    → gitlab (self-hosted by host name)

export interface ParsedRepo {
  provider: 'github' | 'gitlab'
  name: string
  // GitHub
  github_owner: string
  github_repo: string
  // GitLab
  gitlab_project_path: string
}

// Pull "host" and "path" out of the many URL flavours we accept, without relying
// on the URL constructor (which rejects scp-style git@host:path and bare forms).
function splitHostPath(raw: string): { host: string; path: string } | null {
  let s = raw.trim()
  if (!s) return null

  // scp-style: git@host:group/repo.git
  const scp = s.match(/^[\w.-]+@([\w.-]+):(.+)$/)
  if (scp) return { host: scp[1].toLowerCase(), path: scp[2] }

  // strip scheme (https://, http://, ssh://, git://)
  s = s.replace(/^[a-z]+:\/\//i, '')
  // strip userinfo (git@)
  s = s.replace(/^[^@/]+@/, '')

  const slash = s.indexOf('/')
  if (slash === -1) {
    // bare "owner/repo" without a slash isn't possible here; treat as no host
    return { host: '', path: s }
  }
  const host = s.slice(0, slash)
  const path = s.slice(slash + 1)
  // If the first segment looks like a host (has a dot), it's a real host.
  if (host.includes('.')) return { host: host.toLowerCase(), path }
  // Otherwise the whole thing is a shorthand path like "owner/repo".
  return { host: '', path: s }
}

// Build the browser-facing URL for a stored repo, or null if we lack the
// coordinates to do so. Self-hosted GitLab hosts aren't stored, so GitLab links
// assume gitlab.com.
export function repoWebUrl(repo: {
  provider?: 'github' | 'gitlab' | null
  github_owner?: string | null
  github_repo?: string | null
  gitlab_project_path?: string | null
}): string | null {
  const provider = repo.provider ?? 'github'
  if (provider === 'gitlab') {
    const path = repo.gitlab_project_path?.trim()
    return path ? `https://gitlab.com/${path}` : null
  }
  const owner = repo.github_owner?.trim()
  const name = repo.github_repo?.trim()
  return owner && name ? `https://github.com/${owner}/${name}` : null
}

export function parseRepoUrl(raw: string): ParsedRepo | null {
  const split = splitHostPath(raw)
  if (!split) return null

  // Normalise the path: drop query/hash, trailing slash and .git suffix.
  let path = split.path.split(/[?#]/)[0].replace(/\/+$/, '')
  path = path.replace(/\.git$/i, '')
  const segments = path.split('/').filter(Boolean)
  if (segments.length < 2) return null

  const host = split.host
  const isGitlab = host.includes('gitlab')
  const name = segments[segments.length - 1]

  if (isGitlab) {
    return {
      provider: 'gitlab',
      name,
      github_owner: '',
      github_repo: '',
      gitlab_project_path: segments.join('/'),
    }
  }

  // Default to GitHub (github.com, unknown hosts, and bare shorthand).
  return {
    provider: 'github',
    name,
    github_owner: segments[0],
    github_repo: name,
    gitlab_project_path: '',
  }
}
