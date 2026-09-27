import { describe, expect, test } from "vitest";

import {
  freshNativeToastNotices,
  nativeNoticeSeenKey,
  validNativeToastNotice,
} from "./nativeToasts";

describe("native toast notices", () => {
  const notice = {
    id: "toast-request-1",
    code: "insufficient_balance",
    createdAt: 10_000,
    dedupeKey: "insufficient_balance:request-1",
  } as const;

  test("accepts only the allowlisted insufficient-balance notice", () => {
    expect(validNativeToastNotice(notice)).toBe(true);
    expect(validNativeToastNotice({ ...notice, context: "request_blocked" })).toBe(true);
    expect(validNativeToastNotice({ ...notice, context: "local_fallback" })).toBe(false);
    expect(validNativeToastNotice({ ...notice, context: "unknown" })).toBe(false);
    expect(validNativeToastNotice({ ...notice, code: "open_external_url" })).toBe(false);
    expect(validNativeToastNotice({ ...notice, dedupeKey: "" })).toBe(false);
    expect(validNativeToastNotice({
      id: "platform-access-1",
      code: "platform_access_restricted",
      context: "request_blocked",
      createdAt: 10_000,
      dedupeKey: "platform_access_restricted:2030-01-01T00:00:00Z",
      restrictionUntil: "2030-01-01T00:00:00Z",
    })).toBe(true);
    expect(validNativeToastNotice({
      id: "platform-access-legacy",
      code: "platform_access_restricted",
      context: "request_blocked",
      createdAt: 10_000,
      dedupeKey: "platform_access_restricted:unknown",
    })).toBe(true);
    expect(validNativeToastNotice({
      id: "platform-access-invalid",
      code: "platform_access_restricted",
      context: "request_blocked",
      createdAt: 10_000,
      dedupeKey: "platform_access_restricted:invalid",
      restrictionUntil: "not-a-time",
    })).toBe(false);
  });

  test("accepts a bounded platform announcement payload", () => {
    const announcement = {
      id: "announcement-1",
      code: "platform_announcement",
      context: "platform",
      createdAt: 10_000,
      dedupeKey: "platform_announcement",
      version: "announcement_1",
      title: "维护通知",
      body: "服务已经恢复。",
    } as const;
    expect(validNativeToastNotice(announcement)).toBe(true);
    expect(validNativeToastNotice({ ...announcement, context: "request_blocked" })).toBe(false);
    expect(validNativeToastNotice({ ...announcement, body: "x".repeat(8_001) })).toBe(false);
    expect(validNativeToastNotice({
      id: "announcement-clear-1",
      code: "platform_announcement_clear",
      context: "platform",
      createdAt: 10_001,
      dedupeKey: "platform_announcement",
      version: "announcement_1",
    })).toBe(true);
    expect(nativeNoticeSeenKey(announcement)).toBe("platform_announcement:announcement_1");
    expect(nativeNoticeSeenKey({
      id: "announcement-clear-1",
      code: "platform_announcement_clear",
      context: "platform",
      createdAt: 10_001,
      dedupeKey: "platform_announcement",
      version: "announcement_1",
    })).toBe("platform_announcement_clear:announcement_1");
  });

  test("drops stale or future native notices instead of showing old toasts", () => {
    expect(freshNativeToastNotices([notice], 20_000)).toEqual([notice]);
    expect(freshNativeToastNotices([notice], 10_000 + 24 * 60 * 60 * 1_000 + 1)).toEqual([]);
    expect(freshNativeToastNotices([{ ...notice, createdAt: 20_001 }], 20_000)).toEqual([]);
  });
});
