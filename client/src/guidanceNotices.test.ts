import { describe, expect, it } from "vitest";

import {
  activeGuidanceNotice,
  dismissGuidanceNotice,
  guidanceNoticeVisibleOnPage,
  type GuidanceNotice,
  upsertGuidanceNotice,
} from "./guidanceNotices";

function notice(id: string, dedupeKey = id): GuidanceNotice {
  return {
    dedupeKey,
    tone: "info",
    icon: "info",
    title: id,
    body: `${id} body`,
  };
}

describe("guidance notice queue", () => {
  it("updates an active dedupe key without adding another card", () => {
    const current = [notice("newer"), notice("first", "balance")];
    const next = upsertGuidanceNotice(current, {
      ...notice("second", "balance"),
      body: "updated body",
    });

    expect(next).toHaveLength(2);
    expect(next[0]).toMatchObject({ dedupeKey: "balance", title: "second", body: "updated body" });
    expect(next[1].dedupeKey).toBe("newer");
  });

  it("keeps the newest bounded set and allows a dismissed notice to return later", () => {
    const first = upsertGuidanceNotice([], notice("first"), 2);
    const second = upsertGuidanceNotice(first, notice("second"), 2);
    const third = upsertGuidanceNotice(second, notice("third"), 2);
    expect(third.map((item) => item.dedupeKey)).toEqual(["third", "second"]);

    const dismissed = dismissGuidanceNotice(third, "second");
    expect(dismissed.map((item) => item.dedupeKey)).toEqual(["third"]);
    expect(upsertGuidanceNotice(dismissed, notice("second"), 2).map((item) => item.dedupeKey))
      .toEqual(["second", "third"]);
  });

  it("keeps later events at the front so dismissing reveals the previous event", () => {
    const announcement = upsertGuidanceNotice([], notice("announcement"));
    const billing = upsertGuidanceNotice(announcement, notice("billing"));
    expect(billing.map((item) => item.dedupeKey)).toEqual(["billing", "announcement"]);
    expect(dismissGuidanceNotice(billing, "billing")[0].dedupeKey).toBe("announcement");
    expect(activeGuidanceNotice(billing, "access")?.dedupeKey).toBe("billing");
    expect(activeGuidanceNotice(dismissGuidanceNotice(billing, "billing"), "access")?.dedupeKey)
      .toBe("announcement");
  });

  it("keeps global notices visible while page-scoped notices stay on their page", () => {
    expect(guidanceNoticeVisibleOnPage({ ...notice("global"), scope: "global" }, "supplier"))
      .toBe(true);
    expect(guidanceNoticeVisibleOnPage({ ...notice("use"), scope: "access" }, "access"))
      .toBe(true);
    expect(guidanceNoticeVisibleOnPage({ ...notice("use"), scope: "access" }, "supplier"))
      .toBe(false);
  });
});
