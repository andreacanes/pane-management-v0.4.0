/**
 * Convert a Windows path (e.g., C:\Users\USERNAME\project) to a WSL path (/mnt/c/Users/USERNAME/project).
 * Used when sending commands to tmux panes running inside WSL.
 *
 * Shared utility -- imported by App.tsx (onDragEnd), launch.ts, ProjectCard, SessionItem.
 */
export function toWslPath(winPath: string): string {
  const p = winPath.replace(/\\/g, "/");
  if (p.length >= 2 && p[1] === ":") {
    const drive = p[0].toLowerCase();
    return `/mnt/${drive}${p.slice(2)}`;
  }
  return p;
}

/**
 * Convert a WSL path (e.g., /mnt/c/Users/USERNAME/project) to a Windows path (C:\Users\USERNAME\project).
 * Returns the input unchanged if it doesn't match the /mnt/<drive>/... pattern.
 */
export function fromWslPath(wslPath: string): string {
  const match = wslPath.match(/^\/mnt\/([a-z])(\/.*)?$/);
  if (match) {
    const drive = match[1].toUpperCase();
    const rest = (match[2] || "").replace(/\//g, "\\");
    return `${drive}:${rest}`;
  }
  return wslPath;
}

/**
 * Derive a human-readable display name from a project's actual file path.
 * Returns the last path segment (folder name).
 */
export function deriveName(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
  return parts[parts.length - 1] || path;
}

/**
 * Match a pane's current-path against a project's actual_path, tolerating
 * the Windows/WSL divide. Panes report a POSIX-style path from inside WSL
 * (`/mnt/c/Users/...`) while projects store native Windows paths
 * (`C:\Users\...`); compare both normalized forms with trailing slashes
 * stripped. Case-insensitive because Windows filesystems are.
 */
export function pathMatchesProject(panePath: string, actualPath: string): boolean {
  const actualLower = actualPath.toLowerCase().replace(/[\\/]+$/, "");
  const paneLower = panePath.toLowerCase().replace(/\/+$/, "");
  if (actualLower === paneLower) return true;
  if (actualLower === fromWslPath(panePath).toLowerCase().replace(/[\\/]+$/, "")) return true;
  return false;
}

/**
 * Find the project in `projects` whose `actual_path` matches the given
 * pane path. Composed on top of [`pathMatchesProject`]. Generic over the
 * project shape so we don't force `lib/path.ts` to depend on `lib/types.ts`.
 */
export function matchProjectByPath<T extends { actual_path: string }>(
  panePath: string,
  projects: readonly T[],
): T | null {
  if (!panePath) return null;
  return projects.find((p) => pathMatchesProject(panePath, p.actual_path)) ?? null;
}
