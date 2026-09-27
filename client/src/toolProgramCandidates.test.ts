import { describe, expect, it } from "vitest";
import type { ToolProgramCandidate } from "./appTypes";
import {
  compareToolProgramCandidates,
  isToolProgramCandidateLaunchable,
} from "./toolProgramCandidates";

function candidate(
  path: string,
  overrides: Partial<ToolProgramCandidate> = {},
): ToolProgramCandidate {
  return {
    path,
    label: path,
    kind: "test",
    exists: true,
    ...overrides,
  };
}

describe("tool program candidate ordering", () => {
  it("places launchable programs before newer unlaunchable programs", () => {
    const candidates = [
      candidate("new-but-blocked", { launchable: false, modified_at_unix: 300 }),
      candidate("older-and-launchable", { launchable: true, modified_at_unix: 100 }),
    ];

    expect(candidates.sort(compareToolProgramCandidates).map((item) => item.path)).toEqual([
      "older-and-launchable",
      "new-but-blocked",
    ]);
  });

  it("keeps an explicit selection ahead of automatic recommendations", () => {
    const candidates = [
      candidate("recommended", {
        launchable: true,
        recommended: true,
        modified_at_unix: 300,
      }),
      candidate("selected", {
        launchable: true,
        selected: true,
        modified_at_unix: 100,
      }),
    ];

    expect(candidates.sort(compareToolProgramCandidates).map((item) => item.path)).toEqual([
      "selected",
      "recommended",
    ]);
  });

  it("places recommendations ahead of newer ordinary candidates", () => {
    const candidates = [
      candidate("newer", { launchable: true, modified_at_unix: 300 }),
      candidate("recommended", {
        launchable: true,
        recommended: true,
        modified_at_unix: 100,
      }),
    ];

    expect(candidates.sort(compareToolProgramCandidates).map((item) => item.path)).toEqual([
      "recommended",
      "newer",
    ]);
  });

  it("orders otherwise equivalent candidates by modified time descending", () => {
    const candidates = [
      candidate("older", { launchable: true, modified_at_unix: 100 }),
      candidate("newer", { launchable: true, modified_at_unix: 300 }),
      candidate("middle", { launchable: true, modified_at_unix: 200 }),
    ];

    expect(candidates.sort(compareToolProgramCandidates).map((item) => item.path)).toEqual([
      "newer",
      "middle",
      "older",
    ]);
  });

  it("orders known versions before file time and treats a release as newer than its prerelease", () => {
    const candidates = [
      candidate("older", { launchable: true, version: "2.1.9", modified_at_unix: 300 }),
      candidate("prerelease", { launchable: true, version: "2.1.10-rc.2", modified_at_unix: 200 }),
      candidate("release", { launchable: true, version: "2.1.10", modified_at_unix: 100 }),
    ];
    expect(candidates.sort(compareToolProgramCandidates).map((item) => item.path)).toEqual([
      "release", "prerelease", "older",
    ]);
  });

  it("uses a temporary npx cache only after an equally versioned installation", () => {
    const candidates = [
      candidate("C:\\npm\\_npx\\old\\node_modules\\.bin\\dsh.cmd", {
        launchable: true,
        version: "0.1.5-rc.2",
        modified_at_unix: 300,
      }),
      candidate("C:\\npm\\dsh.cmd", {
        launchable: true,
        version: "0.1.5-rc.2",
        modified_at_unix: 100,
      }),
    ];
    expect(candidates.sort(compareToolProgramCandidates)[0].path).toBe("C:\\npm\\dsh.cmd");
  });

  it("falls back to existence when older results do not include launchable", () => {
    expect(isToolProgramCandidateLaunchable(candidate("existing"))).toBe(true);
    expect(
      isToolProgramCandidateLaunchable(candidate("missing", { exists: false })),
    ).toBe(false);
  });
});
