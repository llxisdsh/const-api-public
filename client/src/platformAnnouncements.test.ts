import { beforeEach, describe, expect, it } from "vitest";

import {
  dismissPlatformAnnouncement,
  platformAnnouncementDismissed,
} from "./platformAnnouncements";

describe("platform announcement dismissal", () => {
  beforeEach(() => window.localStorage.clear());

  it("keeps one dismissed version hidden across renderer reloads", () => {
    expect(platformAnnouncementDismissed("announcement_1")).toBe(false);
    dismissPlatformAnnouncement("announcement_1");
    expect(platformAnnouncementDismissed("announcement_1")).toBe(true);
    expect(platformAnnouncementDismissed("announcement_2")).toBe(false);
  });

  it("ignores malformed local preferences", () => {
    window.localStorage.setItem("const-api.dismissed-platform-announcement.v1", "{");
    expect(platformAnnouncementDismissed("announcement_1")).toBe(false);
  });
});
