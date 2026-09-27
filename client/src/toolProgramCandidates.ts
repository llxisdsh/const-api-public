import type { ToolProgramCandidate } from "./appTypes";

export function isToolProgramCandidateLaunchable(candidate: ToolProgramCandidate): boolean {
  return candidate.launchable ?? candidate.exists;
}

function parseKnownVersion(value?: string): { parts: number[]; prerelease: string } | null {
  if (!value) return null;
  const pattern = /^(\d+(?:\.\d+){0,3})(?:-([\da-z.-]+))?(?:\+[\da-z.-]+)?$/i;
  const match = value.match(pattern);
  return match ? { parts: match[1].split(".").map(Number), prerelease: match[2] ?? "" } : null;
}

function compareKnownVersions(
  a: { parts: number[]; prerelease: string },
  b: { parts: number[]; prerelease: string },
): number {
  for (let index = 0; index < Math.max(a.parts.length, b.parts.length); index += 1) {
    const difference = (a.parts[index] ?? 0) - (b.parts[index] ?? 0);
    if (difference) return difference;
  }
  if (!a.prerelease && b.prerelease) return 1;
  if (a.prerelease && !b.prerelease) return -1;
  return a.prerelease.localeCompare(b.prerelease, "en", { numeric: true });
}

export function compareToolProgramCandidates(
  a: ToolProgramCandidate,
  b: ToolProgramCandidate,
): number {
  const aLaunchable = isToolProgramCandidateLaunchable(a);
  const bLaunchable = isToolProgramCandidateLaunchable(b);
  if (aLaunchable !== bLaunchable) return aLaunchable ? -1 : 1;

  if (Boolean(a.selected) !== Boolean(b.selected)) return a.selected ? -1 : 1;
  if (Boolean(a.recommended) !== Boolean(b.recommended)) return a.recommended ? -1 : 1;
  const aVersion = parseKnownVersion(a.version);
  const bVersion = parseKnownVersion(b.version);
  if (Boolean(aVersion) !== Boolean(bVersion)) return aVersion ? -1 : 1;
  const versionDifference = aVersion && bVersion ? compareKnownVersions(bVersion, aVersion) : 0;
  if (versionDifference !== 0) return versionDifference;
  const aCached = a.path.replace(/\\/g, "/").toLowerCase().includes("/_npx/");
  const bCached = b.path.replace(/\\/g, "/").toLowerCase().includes("/_npx/");
  if (aCached !== bCached) return aCached ? 1 : -1;
  const modifiedTimeDifference = (b.modified_at_unix ?? 0) - (a.modified_at_unix ?? 0);
  if (modifiedTimeDifference !== 0) return modifiedTimeDifference;
  return a.path.localeCompare(b.path);
}
