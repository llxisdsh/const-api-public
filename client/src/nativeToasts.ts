import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";

const nativeToastLifetimeMs = 24 * 60 * 60 * 1_000;
const nativeToastSeenLimit = 200;

type InsufficientBalanceNotice = {
  id: string;
  code: "insufficient_balance";
  context?: "request_blocked";
  createdAt: number;
  dedupeKey: string;
};

type PlatformAccessRestrictedNotice = {
  id: string;
  code: "platform_access_restricted";
  context: "request_blocked";
  createdAt: number;
  dedupeKey: string;
  restrictionUntil?: string;
};

export type PlatformAnnouncementNotice = {
  id: string;
  code: "platform_announcement";
  context: "platform";
  createdAt: number;
  dedupeKey: string;
  version: string;
  title: string;
  body: string;
};

type PlatformAnnouncementClearNotice = {
  id: string;
  code: "platform_announcement_clear";
  context: "platform";
  createdAt: number;
  dedupeKey: string;
  version: string;
};

export type NativeToastNotice = InsufficientBalanceNotice | PlatformAccessRestrictedNotice | PlatformAnnouncementNotice | PlatformAnnouncementClearNotice;

function safeString(value: unknown, maximum = 240): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maximum;
}

function safeTimestamp(value: unknown): value is string {
  return safeString(value, 80) && Number.isFinite(Date.parse(value));
}

export function validNativeToastNotice(value: unknown): value is NativeToastNotice {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const notice = value as Partial<NativeToastNotice>;
  if (!safeString(notice.id)
    || !Number.isSafeInteger(notice.createdAt)
    || !safeString(notice.dedupeKey)) return false;
  if (notice.code === "insufficient_balance") {
    return notice.context === undefined || notice.context === "request_blocked";
  }
  if (notice.code === "platform_access_restricted") {
    return notice.context === "request_blocked"
      && (notice.restrictionUntil === undefined || safeTimestamp(notice.restrictionUntil));
  }
  if (notice.code === "platform_announcement_clear") {
    return notice.context === "platform" && safeString(notice.version, 160);
  }
  return notice.code === "platform_announcement"
    && notice.context === "platform"
    && safeString(notice.version, 160)
    && safeString(notice.title, 240)
    && safeString(notice.body, 8_000);
}

export function freshNativeToastNotices(values: unknown[], now = Date.now()): NativeToastNotice[] {
  return values.filter(validNativeToastNotice).filter((notice) => (
    notice.createdAt <= now && now - notice.createdAt <= nativeToastLifetimeMs
  ));
}

export function nativeNoticeSeenKey(notice: NativeToastNotice): string {
  return notice.code === "insufficient_balance" || notice.code === "platform_access_restricted"
    ? notice.dedupeKey
    : `${notice.code}:${notice.version}`;
}

export function useNativeToasts(
  enabled: boolean,
  notify: (notice: NativeToastNotice) => void,
) {
  const notifyRef = useRef(notify);
  const seen = useRef(new Set<string>());
  notifyRef.current = notify;

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const deliver = (values: unknown[]) => {
      if (disposed) return;
      for (const notice of freshNativeToastNotices(values)) {
        const seenKey = nativeNoticeSeenKey(notice);
        if (seen.current.has(seenKey)) continue;
        if (seen.current.size >= nativeToastSeenLimit) seen.current.clear();
        seen.current.add(seenKey);
        notifyRef.current(notice);
      }
    };
    const start = async () => {
      try {
        unlisten = await listen<unknown>("client-guidance-notice", (event) => {
          deliver([event.payload]);
        });
        if (disposed) {
          unlisten();
          unlisten = undefined;
          return;
        }
        const response = await invoke<unknown[]>("drain_client_toast_notices");
        if (Array.isArray(response)) deliver(response);
      } catch {
        // Browser builds and a shutting-down native host have no native notice source.
      }
    };
    void start();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [enabled]);
}
