const dismissedAnnouncementStorageKey = "const-api.dismissed-platform-announcement.v1";
export const platformAnnouncementNoticeKey = "platform_announcement";

type DismissedAnnouncement = {
  version: string;
};

function browserStorage(): Storage | undefined {
  return typeof window === "undefined" ? undefined : window.localStorage;
}

export function platformAnnouncementDismissed(
  version: string,
  storage: Storage | undefined = browserStorage(),
): boolean {
  if (!storage || !version) return false;
  try {
    const parsed = JSON.parse(storage.getItem(dismissedAnnouncementStorageKey) ?? "null") as Partial<DismissedAnnouncement> | null;
    return parsed?.version === version;
  } catch {
    return false;
  }
}

export function dismissPlatformAnnouncement(
  version: string,
  storage: Storage | undefined = browserStorage(),
): void {
  if (!storage || !version) return;
  try {
    storage.setItem(dismissedAnnouncementStorageKey, JSON.stringify({ version } satisfies DismissedAnnouncement));
  } catch {
    // UI preferences are best-effort; closing the active card still succeeds.
  }
}
